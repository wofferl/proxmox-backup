use anyhow::{Error, format_err, bail};
use std::task::{Context, Poll};
use std::os::unix::io::AsRawFd;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use hyper::{Uri, Body};
use hyper::client::{Client, HttpConnector};
use http::{Request, Response, HeaderValue};
use openssl::ssl::{SslConnector, SslMethod};
use futures::*;
use tokio::{
    io::{
        AsyncRead,
        AsyncReadExt,
        AsyncWriteExt,
    },
    net::TcpStream,
};
use tokio_openssl::SslStream;

use crate::tools::{
    async_io::MaybeTlsStream,
    socket::{
        set_tcp_keepalive,
        PROXMOX_BACKUP_TCP_KEEPALIVE_TIME,
    },
};

/// HTTP Proxy Configuration
#[derive(Clone)]
pub struct ProxyConfig {
    pub host: String,
    pub port: u16,
    pub authorization: Option<String>, // Proxy-Authorization header value
    pub force_connect: bool,
}

impl ProxyConfig {

    /// Parse proxy config from ALL_PROXY environment var
    pub fn from_proxy_env() -> Result<Option<ProxyConfig>, Error> {

        // We only support/use ALL_PROXY environment

        match std::env::var_os("ALL_PROXY") {
            None => return Ok(None),
            Some(all_proxy) => {
                let all_proxy = match all_proxy.to_str() {
                    Some(s) => String::from(s),
                    None => bail!("non UTF-8 content in env ALL_PROXY"),
                };
                if all_proxy.is_empty() {
                    return Ok(None);
                }
                let config = Self::parse_proxy_url(&all_proxy)?;
                Ok(Some(config))
            }
        }
    }

    /// Parse proxy configuration string [http://]<host>[:port]
    ///
    /// Default port is 1080 (like curl)
    pub fn parse_proxy_url(http_proxy: &str) -> Result<ProxyConfig, Error> {
        proxmox::try_block!({
            let proxy_uri: Uri = http_proxy.parse()?;
            let proxy_authority = match proxy_uri.authority() {
                Some(authority) => authority,
                None => bail!("missing proxy authority"),
            };
            let host = proxy_authority.host().to_owned();
            let port = match proxy_uri.port() {
                Some(port) => port.as_u16(),
                None => 1080, // CURL default port
            };

            match proxy_uri.scheme_str() {
                Some("http") => { /* Ok */ }
                Some(scheme) => bail!("unsupported proxy scheme '{}'", scheme),
                None => { /* assume HTTP */ }
            }

            let authority_vec: Vec<&str> = proxy_authority.as_str().rsplitn(2, '@').collect();
            let authorization = if authority_vec.len() == 2 {
                Some(format!("Basic {}", base64::encode(authority_vec[1])))
            } else {
                None
            };

            Ok(ProxyConfig {
                host,
                port,
                authorization,
                force_connect: false,
            })
        }).map_err(|err| format_err!("parse_proxy_url failed: {}", err))
    }
}

/// Asyncrounous HTTP client implementation
pub struct SimpleHttp {
    client: Client<HttpsConnector, Body>,
    proxy_authorization: Option<String>, // Proxy-Authorization header value
    user_agent: Option<String>,
}

impl SimpleHttp {

    pub const DEFAULT_USER_AGENT_STRING: &'static str = "proxmox-backup-client/1.0";

    pub fn new(proxy_config: Option<ProxyConfig>) -> Self {
        let ssl_connector = SslConnector::builder(SslMethod::tls()).unwrap().build();
        Self::with_ssl_connector(ssl_connector, proxy_config)
    }

    pub fn with_ssl_connector(ssl_connector: SslConnector, proxy_config: Option<ProxyConfig>) -> Self {

        let mut proxy_authorization = None;
        if let Some(ref proxy_config) = proxy_config {
            if !proxy_config.force_connect {
               proxy_authorization = proxy_config.authorization.clone();
            }
        }

        let connector = HttpConnector::new();
        let mut https = HttpsConnector::with_connector(connector, ssl_connector);
        if let Some(proxy_config) = proxy_config {
            https.set_proxy(proxy_config);
        }
        let client = Client::builder().build(https);
        Self { client, proxy_authorization, user_agent: None }
    }

    pub fn set_user_agent(&mut self, user_agent: &str) -> Result<(), Error> {
        self.user_agent = Some(user_agent.to_owned());
        Ok(())
    }

    fn add_proxy_headers(&self, request: &mut Request<Body>) -> Result<(), Error> {
        if request.uri().scheme() != Some(&http::uri::Scheme::HTTPS) {
            if let Some(ref authorization) = self.proxy_authorization {
                request
                    .headers_mut()
                    .insert(
                        http::header::PROXY_AUTHORIZATION,
                        HeaderValue::from_str(authorization)?,
                    );
            }
        }
        Ok(())
    }

    pub async fn request(&self, mut request: Request<Body>) -> Result<Response<Body>, Error> {
        let user_agent = if let Some(ref user_agent) = self.user_agent {
            HeaderValue::from_str(&user_agent)?
        } else {
            HeaderValue::from_str(Self::DEFAULT_USER_AGENT_STRING)?
        };

        request.headers_mut().insert(hyper::header::USER_AGENT, user_agent);

        self.add_proxy_headers(&mut request)?;

        self.client.request(request)
            .map_err(Error::from)
            .await
    }

    pub async fn post(
        &mut self,
        uri: &str,
        body: Option<String>,
        content_type: Option<&str>,
    ) -> Result<Response<Body>, Error> {

        let body = if let Some(body) = body {
            Body::from(body)
        } else {
            Body::empty()
        };
        let content_type = content_type.unwrap_or("application/json");

        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(hyper::header::CONTENT_TYPE, content_type)
            .body(body)?;

        self.request(request).await
    }

    pub async fn get_string(
        &mut self,
        uri: &str,
        extra_headers: Option<&HashMap<String, String>>,
    ) -> Result<String, Error> {

        let mut request = Request::builder()
            .method("GET")
            .uri(uri);

        if let Some(hs) = extra_headers {
            for (h, v) in hs.iter() {
                request = request.header(h, v);
            }
        }

        let request = request.body(Body::empty())?;

        let res = self.request(request).await?;

        let status = res.status();
        if !status.is_success() {
            bail!("Got bad status '{}' from server", status)
        }

        Self::response_body_string(res).await
    }

    pub async fn response_body_string(res: Response<Body>) -> Result<String, Error> {
        let buf = hyper::body::to_bytes(res).await?;
        String::from_utf8(buf.to_vec())
            .map_err(|err| format_err!("Error converting HTTP result data: {}", err))
    }
}

#[derive(Clone)]
pub struct HttpsConnector {
    connector: HttpConnector,
    ssl_connector: Arc<SslConnector>,
    proxy: Option<ProxyConfig>,
}

impl HttpsConnector {
    pub fn with_connector(mut connector: HttpConnector, ssl_connector: SslConnector) -> Self {
        connector.enforce_http(false);
        Self {
            connector,
            ssl_connector: Arc::new(ssl_connector),
            proxy: None,
        }
    }

    pub fn set_proxy(&mut self, proxy: ProxyConfig) {
        self.proxy = Some(proxy);
    }

    async fn secure_stream(
        tcp_stream: TcpStream,
        ssl_connector: &SslConnector,
        host: &str,
    ) -> Result<MaybeTlsStream<TcpStream>, Error> {
        let config = ssl_connector.configure()?;
        let mut conn: SslStream<TcpStream> = SslStream::new(config.into_ssl(host)?, tcp_stream)?;
        Pin::new(&mut conn).connect().await?;
        Ok(MaybeTlsStream::Secured(conn))
    }

    fn parse_status_line(status_line: &str) -> Result<(), Error> {
        if !(status_line.starts_with("HTTP/1.1 200") || status_line.starts_with("HTTP/1.0 200")) {
            bail!("proxy connect failed - invalid status: {}", status_line)
        }
        Ok(())
    }

    async fn parse_connect_response<R: AsyncRead +  Unpin>(
        stream: &mut R,
    ) -> Result<(), Error> {

        let mut data: Vec<u8> = Vec::new();
        let mut buffer = [0u8; 256];
        const END_MARK: &[u8; 4] = b"\r\n\r\n";

        'outer: loop {
            let n = stream.read(&mut buffer[..]).await?;
            if n == 0 { break; }
            let search_start = if data.len() > END_MARK.len() { data.len() - END_MARK.len() + 1 } else { 0 };
            data.extend(&buffer[..n]);
            if data.len() >= END_MARK.len() {
                if let Some(pos) = data[search_start..].windows(END_MARK.len()).position(|w| w == END_MARK) {
                    let response = String::from_utf8_lossy(&data);
                    let status_line = match response.split("\r\n").next() {
                        Some(status) => status,
                        None => bail!("missing newline"),
                    };
                    Self::parse_status_line(status_line)?;

                    if pos != data.len() - END_MARK.len() {
                        bail!("unexpected data after connect response");
                    }
                    break 'outer;
                }
            }
            if data.len() > 1024*32 { // max 32K (random chosen limit)
                bail!("too many bytes");
            }
        }
        Ok(())
    }
}

impl hyper::service::Service<Uri> for HttpsConnector {
    type Response = MaybeTlsStream<TcpStream>;
    type Error = Error;
    #[allow(clippy::type_complexity)]
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.connector
            .poll_ready(ctx)
            .map_err(|err| err.into())
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let mut connector = self.connector.clone();
        let ssl_connector = Arc::clone(&self.ssl_connector);
        let is_https = dst.scheme() == Some(&http::uri::Scheme::HTTPS);
        let host = match dst.host() {
            Some(host) => host.to_owned(),
            None => {
                return futures::future::err(format_err!("missing URL scheme")).boxed();
            }
        };
        let port = dst.port_u16().unwrap_or(if is_https { 443 } else { 80 });

        if let Some(ref proxy) = self.proxy {

            let use_connect = is_https || proxy.force_connect;

            let proxy_url = format!("{}:{}", proxy.host, proxy.port);
            let proxy_uri = match Uri::builder()
                .scheme("http")
                .authority(proxy_url.as_str())
                .path_and_query("/")
                .build()
            {
                Ok(uri) => uri,
                Err(err) => return futures::future::err(err.into()).boxed(),
            };

            let authorization = proxy.authorization.clone();

            if use_connect {
                async move {

                    let mut tcp_stream = connector
                        .call(proxy_uri)
                        .await
                        .map_err(|err| format_err!("error connecting to {} - {}", proxy_url, err))?;

                    let _ = set_tcp_keepalive(tcp_stream.as_raw_fd(), PROXMOX_BACKUP_TCP_KEEPALIVE_TIME);

                    let mut connect_request = format!("CONNECT {0}:{1} HTTP/1.1\r\n", host, port);
                    if let Some(authorization) = authorization {
                        connect_request.push_str(&format!("Proxy-Authorization: {}\r\n", authorization));
                    }
                    connect_request.push_str(&format!("Host: {0}:{1}\r\n\r\n", host, port));

                    tcp_stream.write_all(connect_request.as_bytes()).await?;
                    tcp_stream.flush().await?;

                    Self::parse_connect_response(&mut tcp_stream).await?;

                    if is_https {
                        Self::secure_stream(tcp_stream, &ssl_connector, &host).await
                    } else {
                        Ok(MaybeTlsStream::Normal(tcp_stream))
                    }
                }.boxed()
            } else {
               async move {
                   let tcp_stream = connector
                       .call(proxy_uri)
                       .await
                       .map_err(|err| format_err!("error connecting to {} - {}", proxy_url, err))?;

                   let _ = set_tcp_keepalive(tcp_stream.as_raw_fd(), PROXMOX_BACKUP_TCP_KEEPALIVE_TIME);

                   Ok(MaybeTlsStream::Proxied(tcp_stream))
               }.boxed()
            }
        } else {
            async move {
                let dst_str = dst.to_string(); // for error messages
                let tcp_stream = connector
                    .call(dst)
                    .await
                    .map_err(|err| format_err!("error connecting to {} - {}", dst_str, err))?;

                let _ = set_tcp_keepalive(tcp_stream.as_raw_fd(), PROXMOX_BACKUP_TCP_KEEPALIVE_TIME);

                if is_https {
                    Self::secure_stream(tcp_stream, &ssl_connector, &host).await
                } else {
                    Ok(MaybeTlsStream::Normal(tcp_stream))
                }
            }.boxed()
        }
    }
}
