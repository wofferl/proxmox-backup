Ext.define('PBS.TapeManagement.TapeRestoreWindow', {
    extend: 'Ext.window.Window',
    alias: 'widget.pbsTapeRestoreWindow',
    mixins: ['Proxmox.Mixin.CBind'],

    title: gettext('Restore Media-Set'),

    width: 800,
    height: 500,

    url: '/api2/extjs/tape/restore',
    method: 'POST',

    modal: true,

    mediaset: undefined,
    prefilter: undefined,
    uuid: undefined,

    cbindData: function(config) {
	let me = this;
	if (me.prefilter !== undefined) {
	    me.title = gettext('Restore Snapshot(s)');
	}
	return {};
    },

    layout: 'fit',
    bodyPadding: 0,

    viewModel: {
	data: {
	    uuid: "",
	    singleDatastore: true,
	},
	formulas: {
	    singleSelectorLabel: get =>
		get('singleDatastore') ? gettext('Target Datastore') : gettext('Default Datastore'),
	    singleSelectorEmptyText: get => get('singleDatastore') ? '' : Proxmox.Utils.NoneText,
	},
    },

    controller: {
	xclass: 'Ext.app.ViewController',

	panelIsValid: function(panel) {
	    return panel.query('[isFormField]').every(field => field.isValid());
	},

	changeMediaSet: function(field, value) {
	    let me = this;
	    let vm = me.getViewModel();
	    vm.set('uuid', value);
	    me.updateSnapshots();
	},

	checkValidity: function() {
	    let me = this;

	    let tabpanel = me.lookup('tabpanel');
	    if (!tabpanel) {
		return; // can get triggered early, when the tabpanel is not yet available
	    }
	    let items = tabpanel.items;

	    let indexOfActiveTab = items.indexOf(tabpanel.getActiveTab());
	    let indexOfLastValidTab = 0;

	    let checkValidity = true;
	    items.each((panel) => {
		if (checkValidity) {
		    panel.setDisabled(false);
		    indexOfLastValidTab = items.indexOf(panel);
		    if (!me.panelIsValid(panel)) {
			checkValidity = false;
		    }
		} else {
		    panel.setDisabled(true);
		}

		return true;
	    });

	    if (indexOfLastValidTab < indexOfActiveTab) {
		tabpanel.setActiveTab(indexOfLastValidTab);
	    } else {
		me.setButtonState(tabpanel.getActiveTab());
	    }
	},

	setButtonState: function(panel) {
	    let me = this;
	    let isValid = me.panelIsValid(panel);
	    let nextButton = me.lookup('nextButton');
	    let finishButton = me.lookup('finishButton');
	    nextButton.setDisabled(!isValid);
	    finishButton.setDisabled(!isValid);
	},

	changeButtonVisibility: function(tabpanel, newItem) {
	    let me = this;
	    let items = tabpanel.items;

	    let backButton = me.lookup('backButton');
	    let nextButton = me.lookup('nextButton');
	    let finishButton = me.lookup('finishButton');

	    let isLast = items.last() === newItem;
	    let isFirst = items.first() === newItem;

	    backButton.setVisible(!isFirst);
	    nextButton.setVisible(!isLast);
	    finishButton.setVisible(isLast);

	    me.setButtonState(newItem);
	},

	previousTab: function() {
	    let me = this;
	    let tabpanel = me.lookup('tabpanel');
	    let index = tabpanel.items.indexOf(tabpanel.getActiveTab());
	    tabpanel.setActiveTab(index - 1);
	},

	nextTab: function() {
	    let me = this;
	    let tabpanel = me.lookup('tabpanel');
	    let index = tabpanel.items.indexOf(tabpanel.getActiveTab());
	    tabpanel.setActiveTab(index + 1);
	},

	getValues: function() {
	    let me = this;

	    let values = {};

	    let tabpanel = me.lookup('tabpanel');
	    tabpanel
		.query('inputpanel')
		.forEach((panel) =>
		    Proxmox.Utils.assemble_field_data(values, panel.getValues()));

	    return values;
	},

	finish: function() {
	    let me = this;
	    let view = me.getView();

	    let values = me.getValues();
	    let url = view.url;
	    let method = view.method;

	    Proxmox.Utils.API2Request({
		url,
		waitMsgTarget: view,
		method,
		params: values,
		failure: function(response, options) {
		    Ext.Msg.alert(gettext('Error'), response.htmlStatus);
		},
		success: function(response, options) {
			// keep around so we can trigger our close events when background action completes
			view.hide();

			Ext.create('Proxmox.window.TaskViewer', {
			    autoShow: true,
			    upid: response.result.data,
			    listeners: {
				destroy: () => view.close(),
			    },
			});
		},
	    });
	},

	updateDatastores: function(grid, values) {
	    let me = this;
	    if (values === 'all') {
		values = [];
	    }
	    let datastores = {};
	    values.forEach((snapshotOrDatastore) => {
		let datastore = snapshotOrDatastore;
		if (snapshotOrDatastore.indexOf(':') !== -1) {
		    let snapshot = snapshotOrDatastore;
		    let match = snapshot.split(':');
		    datastore = match[0];
		} datastores[datastore] = true;
	    });

	    me.setDataStores(Object.keys(datastores));
	},

	setDataStores: function(datastores, initial) {
	    let me = this;

	    // save all datastores on the first setting, and restore them if we selected all
	    if (initial) {
		me.datastores = datastores;
	    } else if (datastores.length === 0) {
		datastores = me.datastores;
	    }

	    const singleDatastore = !datastores || datastores.length <= 1;
	    me.getViewModel().set('singleDatastore', singleDatastore);

	    let grid = me.lookup('mappingGrid');
	    if (!singleDatastore && grid) {
		grid.setDataStores(datastores);
	    }
	},

	updateSnapshots: function() {
	    let me = this;
	    let view = me.getView();
	    let grid = me.lookup('snapshotGrid');
	    let vm = me.getViewModel();
	    let uuid = vm.get('uuid');

	    Proxmox.Utils.API2Request({
		waitMsgTarget: view,
		url: `/tape/media/content?media-set=${uuid}`,
		success: function(response, opt) {
		    let datastores = {};
		    for (const content of response.result.data) {
			datastores[content.store] = true;
		    }
		    me.setDataStores(Object.keys(datastores), true);
		    if (response.result.data.length > 0) {
			grid.setDisabled(false);
			grid.setData(response.result.data);
			grid.getSelectionModel().selectAll();
			// we've shown a big list, center the window again
			view.center();
		    }
		},
		failure: function() {
		    // ignore failing api call, maybe catalog is missing
		    me.setDataStores([], true);
		},
	    });
	},

	init: function(view) {
	    let me = this;
	    let vm = me.getViewModel();

	    vm.set('uuid', view.uuid);
	},

	control: {
	    '[isFormField]': {
		change: 'checkValidity',
		validitychange: 'checkValidity',
	    },
	    'tabpanel': {
		tabchange: 'changeButtonVisibility',
	    },
	},
    },

    buttons: [
	{
	    text: gettext('Back'),
	    reference: 'backButton',
	    handler: 'previousTab',
	    hidden: true,
	},
	{
	    text: gettext('Next'),
	    reference: 'nextButton',
	    handler: 'nextTab',
	},
	{
	    text: gettext('Restore'),
	    reference: 'finishButton',
	    handler: 'finish',
	    hidden: true,
	},
    ],

    items: [
	{
	    xtype: 'tabpanel',
	    reference: 'tabpanel',
	    layout: 'fit',
	    bodyPadding: 10,
	    items: [
		{
		    title: gettext('Snapshot Selection'),
		    xtype: 'inputpanel',
		    onGetValues: function(values) {
			let me = this;

			if (values !== "all" &&
			    Ext.isString(values.snapshots) &&
			    values.snapshots &&
			    values.snapshots.indexOf(':') !== -1
			) {
			    values.snapshots = values.snapshots.split(',');
			} else {
			    delete values.snapshots;
			}

			return values;
		    },

		    column1: [
			{
			    xtype: 'pbsMediaSetSelector',
			    fieldLabel: gettext('Media-Set'),
			    width: 350,
			    submitValue: false,
			    emptyText: gettext('Select Media-Set to restore'),
			    bind: {
				value: '{uuid}',
			    },
			    cbind: {
				hidden: '{uuid}',
				disabled: '{uuid}',
			    },
			    listeners: {
				change: 'changeMediaSet',
			    },
			},
			{
			    xtype: 'displayfield',
			    fieldLabel: gettext('Media-Set'),
			    cbind: {
				value: '{mediaset}',
				hidden: '{!uuid}',
				disabled: '{!uuid}',
			    },
			},
		    ],

		    column2: [
			{
			    xtype: 'displayfield',
			    fieldLabel: gettext('Media-Set UUID'),
			    name: 'media-set',
			    submitValue: true,
			    bind: {
				value: '{uuid}',
				hidden: '{!uuid}',
				disabled: '{!uuid}',
			    },
			},
		    ],

		    columnB: [
			{
			    xtype: 'pbsTapeSnapshotGrid',
			    reference: 'snapshotGrid',
			    name: 'snapshots',
			    height: 322,
			    disabled: true, // will be shown/enabled on successful load
			    listeners: {
				change: 'updateDatastores',
			    },
			    cbind: {
				prefilter: '{prefilter}',
			    },
			},
		    ],
		},
		{
		    title: gettext('Target'),
		    xtype: 'inputpanel',
		    onGetValues: function(values) {
			let me = this;
			let datastores = [];
			if (values.store.toString() !== "") {
			    datastores.push(values.store);
			    delete values.store;
			}

			if (values.mapping.toString() !== "") {
			    datastores.push(values.mapping);
			}
			delete values.mapping;

			values.store = datastores.join(',');

			return values;
		    },
		    column1: [
			{
			    xtype: 'pbsUserSelector',
			    name: 'notify-user',
			    fieldLabel: gettext('Notify User'),
			    emptyText: gettext('Current User'),
			    value: null,
			    allowBlank: true,
			    skipEmptyText: true,
			    renderer: Ext.String.htmlEncode,
			},
			{
			    xtype: 'pbsUserSelector',
			    name: 'owner',
			    fieldLabel: gettext('Owner'),
			    emptyText: gettext('Current User'),
			    value: null,
			    allowBlank: true,
			    skipEmptyText: true,
			    renderer: Ext.String.htmlEncode,
			},
		    ],

		    column2: [
			{
			    xtype: 'pbsDriveSelector',
			    name: 'drive',
			    fieldLabel: gettext('Drive'),
			    labelWidth: 120,
			},
			{
			    xtype: 'pbsDataStoreSelector',
			    name: 'store',
			    labelWidth: 120,
			    bind: {
				fieldLabel: '{singleSelectorLabel}',
				emptyText: '{singleSelectorEmptyText}',
				allowBlank: '{!singleDatastore}',
			    },
			    listeners: {
				change: function(field, value) {
				    this.up('window').lookup('mappingGrid').setNeedStores(!value);
				},
			    },
			},
		    ],

		    columnB: [
			{
			    xtype: 'displayfield',
			    fieldLabel: gettext('Datastore Mapping'),
			    labelWidth: 200,
			    bind: {
				hidden: '{singleDatastore}',
			    },
			},
			{
			    xtype: 'pbsDataStoreMappingField',
			    name: 'mapping',
			    reference: 'mappingGrid',
			    height: 260,
			    defaultBindProperty: 'value',
			    bind: {
				hidden: '{singleDatastore}',
			    },
			},
		    ],
		},
	    ],
	},
    ],

    listeners: {
	afterrender: 'updateSnapshots',
    },
});

Ext.define('PBS.TapeManagement.DataStoreMappingGrid', {
    extend: 'Ext.grid.Panel',
    alias: 'widget.pbsDataStoreMappingField',
    mixins: ['Ext.form.field.Field'],

    scrollable: true,

    getValue: function() {
	let me = this;
	let datastores = [];
	me.getStore().each(rec => {
	    let { source, target } = rec.data;
	    if (target && target !== "") {
		datastores.push(`${source}=${target}`);
	    }
	});

	return datastores.join(',');
    },

    viewModel: {
	data: {
	    needStores: false, // this determines if we need at least one valid mapping
	},
	formulas: {
	    emptyMeans: get => get('needStores') ? Proxmox.Utils.NoneText : Proxmox.Utils.defaultText,
	},
    },

    setNeedStores: function(needStores) {
	let me = this;
	me.getViewModel().set('needStores', needStores);
	me.checkChange();
	me.validate();
    },

    setValue: function(value) {
	let me = this;
	me.setDataStores(value);
	return me;
    },

    getErrors: function(value) {
	let me = this;
	let error = false;

	if (me.getViewModel().get('needStores')) {
	    error = true;
	    me.getStore().each(rec => {
		if (rec.data.target) {
		    error = false;
		}
	    });
	}

	let el = me.getActionEl();
	if (error) {
	    me.addCls(['x-form-trigger-wrap-default', 'x-form-trigger-wrap-invalid']);
	    let errorMsg = gettext("Need at least one mapping");
	    if (el) {
		el.dom.setAttribute('data-errorqtip', errorMsg);
	    }

	    return [errorMsg];
	}
	me.removeCls(['x-form-trigger-wrap-default', 'x-form-trigger-wrap-invalid']);
	if (el) {
	    el.dom.setAttribute('data-errorqtip', "");
	}
	return [];
    },

    setDataStores: function(datastores) {
	let me = this;

	let data = [];
	for (const datastore of datastores) {
	    data.push({
		source: datastore,
		target: '',
	    });
	}

	me.getStore().setData(data);
    },

    viewConfig: {
	markDirty: false,
    },

    store: { data: [] },

    columns: [
	{
	    text: gettext('Source Datastore'),
	    dataIndex: 'source',
	    flex: 1,
	},
	{
	    text: gettext('Target Datastore'),
	    xtype: 'widgetcolumn',
	    dataIndex: 'target',
	    flex: 1,
	    widget: {
		xtype: 'pbsDataStoreSelector',
		allowBlank: true,
		bind: {
		    emptyText: '{emptyMeans}',
		},
		listeners: {
		    change: function(selector, value) {
			let me = this;
			let rec = me.getWidgetRecord();
			if (!rec) {
			    return;
			}
			rec.set('target', value);
			me.up('grid').checkChange();
		    },
		},
	    },
	},
    ],
});

Ext.define('PBS.TapeManagement.SnapshotGrid', {
    extend: 'Ext.grid.Panel',
    alias: 'widget.pbsTapeSnapshotGrid',
    mixins: ['Ext.form.field.Field'],

    getValue: function() {
	let me = this;
	let snapshots = [];

	let storeCounts = {};

	me.getSelection().forEach((rec) => {
	    let id = rec.get('id');
	    let store = rec.data.store;
	    let snap = rec.data.snapshot;
	    // only add if not filtered
	    if (me.store.findExact('id', id) !== -1) {
		snapshots.push(`${store}:${snap}`);
		if (storeCounts[store] === undefined) {
		    storeCounts[store] = 0;
		}
		storeCounts[store]++;
	    }
	});

	// getSource returns null if data is not filtered
	let originalData = me.store.getData().getSource() || me.store.getData();

	if (snapshots.length === originalData.length) {
	    return "all";
	}

	let wholeStores = [];
	let wholeStoresSelected = true;
	for (const [store, count] of Object.entries(storeCounts)) {
	    if (me.storeCounts[store] === count) {
		wholeStores.push(store);
	    } else {
		wholeStoresSelected = false;
		break;
	    }
	}

	if (wholeStoresSelected) {
	    return wholeStores;
	}

	return snapshots;
    },

    setValue: function(value) {
	let me = this;
	// not implemented
	return me;
    },

    getErrors: function(value) {
	let me = this;
	if (me.getSelection().length < 1) {
	    me.addCls(['x-form-trigger-wrap-default', 'x-form-trigger-wrap-invalid']);
	    let errorMsg = gettext("Need at least one snapshot");
	    let el = me.getActionEl();
	    if (el) {
		el.dom.setAttribute('data-errorqtip', errorMsg);
	    }

	    return [errorMsg];
	}
	me.removeCls(['x-form-trigger-wrap-default', 'x-form-trigger-wrap-invalid']);
	let el = me.getActionEl();
	if (el) {
	    el.dom.setAttribute('data-errorqtip', "");
	}
	return [];
    },

    setData: function(records) {
	let me = this;
	let storeCounts = {};
	records.forEach((rec) => {
	    let store = rec.store;
	    if (storeCounts[store] === undefined) {
		storeCounts[store] = 0;
	    }
	    storeCounts[store]++;
	});
	me.storeCounts = storeCounts;
	me.getStore().setData(records);
    },

    scrollable: true,
    plugins: 'gridfilters',

    viewConfig: {
	emptyText: gettext('No Snapshots'),
	markDirty: false,
    },

    selModel: 'checkboxmodel',
    store: {
	sorters: ['store', 'snapshot'],
	data: [],
	filters: [],
    },

    listeners: {
	selectionchange: function() {
	    // to trigger validity and error checks
	    this.checkChange();
	},
    },

    checkChangeEvents: [
	'selectionchange',
	'change',
    ],

    columns: [
	{
	    text: gettext('Source Datastore'),
	    dataIndex: 'store',
	    filter: {
		type: 'list',
	    },
	    flex: 1,
	},
	{
	    text: gettext('Snapshot'),
	    dataIndex: 'snapshot',
	    filter: {
		type: 'string',
	    },
	    flex: 2,
	},
    ],

    initComponent: function() {
	let me = this;
	me.callParent();
	if (me.prefilter !== undefined) {
	    if (me.prefilter.store !== undefined) {
		me.store.filters.add(
		    {
			id: 'x-gridfilter-store',
			property: 'store',
			operator: 'in',
			value: [me.prefilter.store],
		    },
		);
	    }

	    if (me.prefilter.snapshot !== undefined) {
		me.store.filters.add(
		    {
			id: 'x-gridfilter-snapshot',
			property: 'snapshot',
			value: me.prefilter.snapshot,
		    },
		);
	    }
	}

	me.mon(me.store, 'filterchange', () => me.checkChange());
    },
});

Ext.define('PBS.TapeManagement.MediaSetSelector', {
    extend: 'Proxmox.form.ComboGrid',
    alias: 'widget.pbsMediaSetSelector',

    allowBlank: false,
    displayField: 'media-set-name',
    valueField: 'media-set-uuid',
    autoSelect: false,

    store: {
	proxy: {
	    type: 'proxmox',
	    url: '/api2/json/tape/media/media-sets',
	},
	autoLoad: true,
	idProperty: 'media-set-uuid',
	sorters: ['pool', 'media-set-ctime'],
    },

    listConfig: {
	width: 600,
	columns: [
	    {
		text: gettext('Pool'),
		dataIndex: 'pool',
		flex: 1,
	    },
	    {
		text: gettext('Name'),
		dataIndex: 'media-set-name',
		width: 180,
	    },
	    {
		text: gettext('Media-Set UUID'),
		dataIndex: 'media-set-uuid',
		width: 280,
	    },
	],
    },
});
