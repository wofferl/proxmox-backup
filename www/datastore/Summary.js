Ext.define('pve-rrd-datastore', {
    extend: 'Ext.data.Model',
    fields: [
	'used',
	'total',
	'read_ios',
	'read_bytes',
	'write_ios',
	'write_bytes',
	'io_ticks',
	{
	    name: 'io_delay', calculate: function(data) {
		let ios = 0;
		if (data.read_ios !== undefined) { ios += data.read_ios; }
		if (data.write_ios !== undefined) { ios += data.write_ios; }
		if (data.io_ticks === undefined) {
		    return undefined;
		} else if (ios === 0) {
		    return 0;
		}
		return (data.io_ticks*1000.0)/ios;
	    },
	},
	{ type: 'date', dateFormat: 'timestamp', name: 'time' },
    ],
});

Ext.define('PBS.DataStoreInfo', {
    extend: 'Ext.panel.Panel',
    alias: 'widget.pbsDataStoreInfo',

    viewModel: {
	data: {
	    countstext: '',
	    usage: {},
	    stillbad: 0,
	    mountpoint: "",
	},
    },

    controller: {
	xclass: 'Ext.app.ViewController',

	onLoad: function(store, data, success) {
	    if (!success) return;
	    let me = this;
	    let vm = me.getViewModel();

	    let counts = store.getById('counts').data.value;
	    let total = store.getById('total').data.value;
	    let used = store.getById('used').data.value;

	    let usage = PBS.Utils.render_size_usage(used, total);
	    vm.set('usagetext', usage);
	    vm.set('usage', used/total);

	    let gcstatus = store.getById('gc-status').data.value;

	    let dedup = PBS.Utils.calculate_dedup_factor(gcstatus);

	    let countstext = function(count) {
		count = count || {};
		return `${count.groups || 0} ${gettext('Groups')}, ${count.snapshots || 0} ${gettext('Snapshots')}`;
	    };

	    vm.set('ctcount', countstext(counts.ct));
	    vm.set('vmcount', countstext(counts.vm));
	    vm.set('hostcount', countstext(counts.host));
	    vm.set('deduplication', dedup.toFixed(2));
	    vm.set('stillbad', gcstatus['still-bad']);
	},

	startStore: function() { this.store.startUpdate(); },
	stopStore: function() { this.store.stopUpdate(); },

	init: function(view) {
	    let me = this;
	    let datastore = encodeURIComponent(view.datastore);
	    me.store = Ext.create('Proxmox.data.ObjectStore', {
		interval: 5*1000,
		url: `/api2/json/admin/datastore/${datastore}/status/?verbose=true`,
	    });
	    me.store.on('load', me.onLoad, me);
	},
    },

    listeners: {
	activate: 'startStore',
	beforedestroy: 'stopStore',
	deactivate: 'stopStore',
    },

    defaults: {
	xtype: 'pmxInfoWidget',
    },

    bodyPadding: 20,

    items: [
	{
	    iconCls: 'fa fa-fw fa-hdd-o',
	    title: gettext('Usage'),
	    bind: {
		data: {
		    usage: '{usage}',
		    text: '{usagetext}',
		},
	    },
	},
	{
	    xtype: 'box',
	    html: `<b>${gettext('Backup Count')}</b>`,
	    padding: '10 0 5 0',
	},
	{
	    iconCls: 'fa fa-fw fa-cube',
	    title: gettext('CT'),
	    printBar: false,
	    bind: {
		data: {
		    text: '{ctcount}',
		},
	    },
	},
	{
	    iconCls: 'fa fa-fw fa-building',
	    title: gettext('Host'),
	    printBar: false,
	    bind: {
		data: {
		    text: '{hostcount}',
		},
	    },
	},
	{
	    iconCls: 'fa fa-fw fa-desktop',
	    title: gettext('VM'),
	    printBar: false,
	    bind: {
		data: {
		    text: '{vmcount}',
		},
	    },
	},
	{
	    xtype: 'box',
	    html: `<b>${gettext('Stats from last Garbage Collection')}</b>`,
	    padding: '10 0 5 0',
	},
	{
	    iconCls: 'fa fa-fw fa-compress',
	    title: gettext('Deduplication Factor'),
	    printBar: false,
	    bind: {
		data: {
		    text: '{deduplication}',
		},
	    },
	},
	{
	    iconCls: 'fa critical fa-fw fa-exclamation-triangle',
	    title: gettext('Bad Chunks'),
	    printBar: false,
	    bind: {
		data: {
		    text: '{stillbad}',
		},
		visible: '{stillbad}',
	    },
	},
    ],
});

Ext.define('PBS.DataStoreSummary', {
    extend: 'Ext.panel.Panel',
    alias: 'widget.pbsDataStoreSummary',
    mixins: ['Proxmox.Mixin.CBind'],

    layout: 'column',
    scrollable: true,

    bodyPadding: 5,
    defaults: {
	columnWidth: 1,
	padding: 5,
    },

    tbar: ['->', { xtype: 'proxmoxRRDTypeSelector' }],

    items: [
	{
	    xtype: 'container',
	    height: 300,
	    layout: {
		type: 'hbox',
		align: 'stretch',
	    },
	    items: [
		{
		    xtype: 'pbsDataStoreInfo',
		    flex: 1,
		    padding: '0 10 0 0',
		    cbind: {
			title: '{datastore}',
			datastore: '{datastore}',
		    },
		},
		{
		    xtype: 'pbsDataStoreNotes',
		    flex: 1,
		    cbind: {
			datastore: '{datastore}',
		    },
		},
	    ],
	},
	{
	    xtype: 'proxmoxRRDChart',
	    title: gettext('Storage usage (bytes)'),
	    fields: ['total', 'used'],
	    fieldTitles: [gettext('Total'), gettext('Storage usage')],
	},
	{
	    xtype: 'proxmoxRRDChart',
	    title: gettext('Transfer Rate (bytes/second)'),
	    fields: ['read_bytes', 'write_bytes'],
	    fieldTitles: [gettext('Read'), gettext('Write')],
	},
	{
	    xtype: 'proxmoxRRDChart',
	    title: gettext('Input/Output Operations per Second (IOPS)'),
	    fields: ['read_ios', 'write_ios'],
	    fieldTitles: [gettext('Read'), gettext('Write')],
	},
	{
	    xtype: 'proxmoxRRDChart',
	    title: gettext('IO Delay (ms)'),
	    fields: ['io_delay'],
	    fieldTitles: [gettext('IO Delay')],
	},
    ],

    listeners: {
	activate: function() { this.rrdstore.startUpdate(); },
	deactivate: function() { this.rrdstore.stopUpdate(); },
	destroy: function() { this.rrdstore.stopUpdate(); },
    },

    initComponent: function() {
	let me = this;

	me.rrdstore = Ext.create('Proxmox.data.RRDStore', {
	    rrdurl: "/api2/json/admin/datastore/" + me.datastore + "/rrd",
	    model: 'pve-rrd-datastore',
	});

	me.callParent();

	Proxmox.Utils.API2Request({
	    url: `/config/datastore/${me.datastore}`,
	    waitMsgTarget: me.down('pbsDataStoreInfo'),
	    success: function(response) {
		let path = Ext.htmlEncode(response.result.data.path);
		me.down('pbsDataStoreInfo').setTitle(`${me.datastore} (${path})`);
		me.down('pbsDataStoreNotes').setNotes(response.result.data.comment);
	    },
	    failure: function(response) {
		// fallback if e.g. we have no permissions to the config
		let rec = Ext.getStore('pbs-datastore-list')
		    .findRecord('store', me.datastore, 0, false, true, true);
		if (rec) {
		    me.down('pbsDataStoreNotes').setNotes(rec.data.comment || "");
		}
	    },
	});

	me.query('proxmoxRRDChart').forEach((chart) => {
	    chart.setStore(me.rrdstore);
	});

	me.down('pbsDataStoreInfo').relayEvents(me, ['activate', 'deactivate']);
    },
});
