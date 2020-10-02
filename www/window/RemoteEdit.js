Ext.define('PBS.window.RemoteEdit', {
    extend: 'Proxmox.window.Edit',
    alias: 'widget.pbsRemoteEdit',
    mixins: ['Proxmox.Mixin.CBind'],

    onlineHelp: 'backup_remote',

    userid: undefined,

    isAdd: true,

    subject: gettext('Remote'),

    fieldDefaults: { labelWidth: 120 },

    cbindData: function(initialConfig) {
	let me = this;

	let baseurl = '/api2/extjs/config/remote';
	let name = initialConfig.name;

	me.isCreate = !name;
	me.url = name ? `${baseurl}/${name}` : baseurl;
	me.method = name ? 'PUT' : 'POST';
	me.autoLoad = !!name;
	return {
	    passwordEmptyText: me.isCreate ? '' : gettext('Unchanged'),
	};
    },

    items: {
	xtype: 'inputpanel',
	column1: [
	    {
		xtype: 'pmxDisplayEditField',
		name: 'name',
		fieldLabel: gettext('Remote'),
		renderer: Ext.htmlEncode,
		allowBlank: false,
		minLength: 4,
		cbind: {
		    editable: '{isCreate}',
		},
	    },
	    {
		xtype: 'proxmoxtextfield',
		allowBlank: false,
		name: 'hostport',
		submitValue: false,
		vtype: 'HostPort',
		fieldLabel: gettext('Host'),
		listeners: {
		    change: function(field, newvalue) {
			let host = newvalue;
			let port;

			let match = Proxmox.Utils.HostPort_match.exec(newvalue);
			if (match === null) {
			    match = Proxmox.Utils.HostPortBrackets_match.exec(newvalue);
			    if (match === null) {
				match = Proxmox.Utils.IP6_dotnotation_match.exec(newvalue);
			    }
			}

			if (match !== null) {
			    host = match[1];
			    if (match[2] !== undefined) {
				port = match[2];
			    }
			}

			field.up('inputpanel').down('field[name=host]').setValue(host);
			field.up('inputpanel').down('field[name=port]').setValue(port);
		    },
		},
	    },
	    {
		xtype: 'proxmoxtextfield',
		hidden: true,
		name: 'host',
	    },
	    {
		xtype: 'proxmoxtextfield',
		hidden: true,
		cbind: {
		    deleteEmpty: '{!isCreate}',
		},
		name: 'port',
	    },
	],

	column2: [
	    {
		xtype: 'proxmoxtextfield',
		allowBlank: false,
		name: 'userid',
		fieldLabel: gettext('Userid'),
	    },
	    {
		xtype: 'textfield',
		inputType: 'password',
		fieldLabel: gettext('Password'),
		name: 'password',
		cbind: {
		    emptyText: '{passwordEmptyText}',
		    allowBlank: '{!isCreate}',
		},
	    },
	],

	columnB: [
	    {
		xtype: 'proxmoxtextfield',
		name: 'fingerprint',
		cbind: {
		    deleteEmpty: '{!isCreate}',
		},
		fieldLabel: gettext('Fingerprint'),
	    },
	    {
		xtype: 'proxmoxtextfield',
		name: 'comment',
		cbind: {
		    deleteEmpty: '{!isCreate}',
		},
		fieldLabel: gettext('Comment'),
	    },
	],
    },

    setValues: function(values) {
	let me = this;

	let host = values.host;
	if (values.port !== undefined) {
	    if (Proxmox.Utils.IP6_match.test(host)) {
		host = `[${host}]`;
	    }
	    host += `:${values.port}`;
	}
	values.hostport = host;

	return me.callParent([values]);
    },

    getValues: function() {
	let me = this;
	let values = me.callParent(arguments);

	if (values.password === '') {
	    delete values.password;
	}

	return values;
    },
});
