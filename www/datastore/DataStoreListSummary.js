// Summary Panel for a single datastore in overview
Ext.define('PBS.datastore.DataStoreListSummary', {
    extend: 'Ext.panel.Panel',
    alias: 'widget.pbsDataStoreListSummary',
    mixins: ['Proxmox.Mixin.CBind'],

    cbind: {
	title: '{datastore}',
    },

    referenceHolder: true,
    bodyPadding: 10,

    layout: {
	type: 'hbox',
	align: 'stretch',
    },

    viewModel: {
	data: {
	    full: "N/A",
	    stillbad: 0,
	    deduplication: 1.0,
	    error: "",
	},
    },
    setTasks: function(taskdata, since) {
	let me = this;
	me.down('pbsTaskSummary').updateTasks(taskdata, since);
    },

    setStatus: function(statusData) {
	let me = this;
	let vm = me.getViewModel();

	if (statusData.error !== undefined) {
	    vm.set('error', statusData.error);
	    return;
	} else {
	    vm.set('error', "");
	}

	let usage = statusData.used/statusData.total;
	let usagetext = Ext.String.format(gettext('{0} of {1}'),
	    Proxmox.Utils.format_size(statusData.used),
	    Proxmox.Utils.format_size(statusData.total),
	);

	let usagePanel = me.lookup('usage');
	usagePanel.updateValue(usage, usagetext);

	let estimate = PBS.Utils.render_estimate(statusData['estimated-full-date']);
	vm.set('full', estimate);
	vm.set('deduplication', PBS.Utils.calculate_dedup_factor(statusData['gc-status']).toFixed(2));
	vm.set('stillbad', statusData['gc-status']['still-bad']);

	let last = 0;
	let time = statusData['history-start'];
	let delta = statusData['history-delta'];
	let data = statusData.history.map((val) => {
	    if (val === null) {
		val = last;
	    } else {
		last = val;
	    }
	    let entry = {
		time: time*1000, // js Dates are ms since epoch
		val,
	    };

	    time += delta;
	    return entry;
	});

	me.lookup('historychart').setData(data);
    },

    items: [
	{
	    xtype: 'container',
	    layout: {
		type: 'vbox',
		align: 'stretch',
	    },

	    width: 375,
	    padding: '5 25 5 5',

	    defaults: {
		padding: 2,
	    },

	    items: [
		{
		    xtype: 'box',
		    reference: 'errorBox',
		    hidden: true,
		    tpl: [
			'<center>',
			`<h3>${gettext("Error")}</h3>`,
			'<i class="fa fa-5x fa-exclamation-circle critical"></i>',
			'<br /><br/>',
			'{text}',
			'</center>',
		    ],
		    bind: {
			visible: '{error}',
			data: {
			    text: '{error}',
			},
		    },
		},
		{
		    xtype: 'proxmoxGauge',
		    warningThreshold: 0.8,
		    criticalThreshold: 0.95,
		    flex: 1,
		    reference: 'usage',
		    bind: {
			visible: '{!error}',
		    },
		},
		{
		    xtype: 'pmxInfoWidget',
		    iconCls: 'fa fa-fw fa-line-chart',
		    title: gettext('Estimated Full'),
		    printBar: false,
		    bind: {
			data: {
			    text: '{full}',
			},
			visible: '{!error}',
		    },
		},
		{
		    xtype: 'pmxInfoWidget',
		    iconCls: 'fa fa-fw fa-compress',
		    title: gettext('Deduplication Factor'),
		    printBar: false,
		    bind: {
			data: {
			    text: '{deduplication}',
			},
			visible: '{!error}',
		    },
		},
		{
		    xtype: 'pmxInfoWidget',
		    iconCls: 'fa critical fa-fw fa-exclamation-triangle',
		    title: gettext('Bad Chunks'),
		    printBar: false,
		    hidden: true,
		    bind: {
			data: {
			    text: '{stillbad}',
			},
			visible: '{stillbad}',
		    },
		},
	    ],
	},
	{
	    xtype: 'container',
	    layout: {
		type: 'vbox',
		align: 'stretch',
	    },

	    flex: 1,

	    items: [
		{
		    padding: 5,
		    xtype: 'pbsUsageChart',
		    reference: 'historychart',
		    title: gettext('Usage History'),
		    height: 100,
		    bind: {
			visible: '{!error}',
		    },
		},
		{
		    xtype: 'container',
		    flex: 1,
		    layout: {
			type: 'vbox',
			align: 'stretch',
		    },

		    defaults: {
			padding: 5,
		    },

		    items: [
			{
			    xtype: 'label',
			    text: gettext('Task Summary')
			        + ` (${Ext.String.format(gettext('{0} days'), 30)})`,
			},
			{
			    xtype: 'pbsTaskSummary',
			    border: false,
			    header: false,
			    subPanelModal: true,
			    flex: 2,
			    bodyPadding: 0,
			    minHeight: 0,
			    cbind: {
				datastore: '{datastore}',
			    },
			},
		    ],
		},
	    ],
	},
    ],
});
