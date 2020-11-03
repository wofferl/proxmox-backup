// avoid errors when running without development tools
if (!Ext.isDefined(Ext.global.console)) {
    var console = {
        dir: function() {},
        log: function() {},
    };
}

Ext.onReady(function() {
    const NOW = new Date();
    const COLORS = {
	'keep-last': 'orange',
	'keep-hourly': 'purple',
	'keep-daily': 'yellow',
	'keep-weekly': 'green',
	'keep-monthly': 'blue',
	'keep-yearly': 'red',
	'all zero': 'white',
    };
    const TEXT_COLORS = {
	'keep-last': 'black',
	'keep-hourly': 'white',
	'keep-daily': 'black',
	'keep-weekly': 'white',
	'keep-monthly': 'white',
	'keep-yearly': 'white',
	'all zero': 'black',
    };

    Ext.define('PBS.prunesimulator.Documentation', {
	extend: 'Ext.Panel',
	alias: 'widget.prunesimulatorDocumentation',

	html: '<iframe style="width:100%;height:100%" src="./documentation.html"/>',
    });

    Ext.define('PBS.prunesimulator.CalendarEvent', {
	extend: 'Ext.form.field.ComboBox',
	alias: 'widget.prunesimulatorCalendarEvent',

	editable: true,

	displayField: 'text',
	valueField: 'value',
	queryMode: 'local',

	store: {
	    field: ['value', 'text'],
	    data: [
		{ value: '0/2:00', text: "Every two hours" },
		{ value: '0/6:00', text: "Every six hours" },
		{ value: '2,22:30', text: "At 02:30 and 22:30" },
		{ value: '08..17:00/30', text: "From 08:00 to 17:30 every 30 minutes" },
		{ value: 'HOUR:MINUTE', text: "Custom schedule" },
	    ],
	},

	tpl: [
	    '<ul class="x-list-plain"><tpl for=".">',
	    '<li role="option" class="x-boundlist-item">{text}</li>',
	    '</tpl></ul>',
	],

	displayTpl: [
	    '<tpl for=".">',
	    '{value}',
	    '</tpl>',
	],
    });

    Ext.define('PBS.prunesimulator.DayOfWeekSelector', {
	extend: 'Ext.form.field.ComboBox',
	alias: 'widget.prunesimulatorDayOfWeekSelector',

	editable: false,

	displayField: 'text',
	valueField: 'value',
	queryMode: 'local',

	store: {
	    field: ['value', 'text'],
	    data: [
		{ value: 'mon', text: Ext.util.Format.htmlDecode(Ext.Date.dayNames[1]) },
		{ value: 'tue', text: Ext.util.Format.htmlDecode(Ext.Date.dayNames[2]) },
		{ value: 'wed', text: Ext.util.Format.htmlDecode(Ext.Date.dayNames[3]) },
		{ value: 'thu', text: Ext.util.Format.htmlDecode(Ext.Date.dayNames[4]) },
		{ value: 'fri', text: Ext.util.Format.htmlDecode(Ext.Date.dayNames[5]) },
		{ value: 'sat', text: Ext.util.Format.htmlDecode(Ext.Date.dayNames[6]) },
		{ value: 'sun', text: Ext.util.Format.htmlDecode(Ext.Date.dayNames[0]) },
	    ],
	},
    });

    Ext.define('pbs-prune-list', {
	extend: 'Ext.data.Model',
	fields: [
	    {
		name: 'backuptime',
		type: 'date',
		dateFormat: 'timestamp',
	    },
	    {
		name: 'mark',
		type: 'string',
	    },
	    {
		name: 'keepName',
		type: 'string',
	    },
	],
    });

    Ext.define('PBS.prunesimulator.PruneList', {
	extend: 'Ext.panel.Panel',
	alias: 'widget.prunesimulatorPruneList',

	initComponent: function() {
	    var me = this;

	    if (!me.store) {
		throw "no store specified";
	    }

	    me.items = [
		{
		    xtype: 'grid',
		    store: me.store,
		    columns: [
			{
			    header: 'Backup Time',
			    dataIndex: 'backuptime',
			    renderer: function(value, metaData, record) {
				let text = Ext.Date.format(value, 'Y-m-d H:i:s');
				if (record.data.mark === 'keep') {
				    if (me.useColors) {
					let bgColor = COLORS[record.data.keepName];
					let textColor = TEXT_COLORS[record.data.keepName];
					return '<div style="background-color: ' + bgColor + '; ' +
							    'color: ' + textColor + ';">' + text + '</div>';
				    } else {
					return text;
				    }
				} else {
				    return '<div style="text-decoration: line-through;">' + text + '</div>';
				}
			    },
			    flex: 1,
			    sortable: false,
			},
			{
			    header: 'Keep (reason)',
			    dataIndex: 'mark',
			    renderer: function(value, metaData, record) {
				if (record.data.mark === 'keep') {
				    return 'keep (' + record.data.keepName + ')';
				} else {
				    return value;
				}
			    },
			    width: 200,
			    sortable: false,
			},
		    ],
		},
	    ];

	    me.callParent();
	},
    });

    Ext.define('PBS.prunesimulator.WeekTable', {
	extend: 'Ext.panel.Panel',
	alias: 'widget.prunesimulatorWeekTable',

	reload: function() {
	    let me = this;
	    let backups = me.store.data.items;

	    let html = '<table>';

	    let now = new Date(NOW.getTime());
	    let skip = 7 - parseInt(Ext.Date.format(now, 'N'), 10);
	    let tableStartDate = Ext.Date.add(now, Ext.Date.DAY, skip);

	    let bIndex = 0;

	    for (let i = 0; bIndex < backups.length; i++) {
		html += '<tr>';

		for (let j = 0; j < 7; j++) {
		    html += '<td style="vertical-align: top;' +
				       'width: 150px;' +
				       'border: black 1px solid;' +
				       '">';

		    let date = Ext.Date.subtract(tableStartDate, Ext.Date.DAY, j + 7 * i);
		    let currentDay = Ext.Date.format(date, 'd/m/Y');

		    let isBackupOnDay = function(backup, day) {
			return backup && Ext.Date.format(backup.data.backuptime, 'd/m/Y') === day;
		    };

		    let backup = backups[bIndex];

		    html += '<table><tr><th style="border-bottom: black 1px solid;">' +
			    Ext.Date.format(date, 'D, d M Y') + '</th>';

		    while (isBackupOnDay(backup, currentDay)) {
			html += '<tr><td>';

			let text = Ext.Date.format(backup.data.backuptime, 'H:i');
			if (backup.data.mark === 'remove') {
			    html += '<div style="text-decoration: line-through;">' + text + '</div>';
			} else {
			    text += ' (' + backup.data.keepName + ')';
			    if (me.useColors) {
				let bgColor = COLORS[backup.data.keepName];
				let textColor = TEXT_COLORS[backup.data.keepName];
				html += '<div style="background-color: ' + bgColor + '; ' +
						    'color: ' + textColor + ';">' + text + '</div>';
			    } else {
				html += '<div>' + text + '</div>';
			    }
			}
			html += '</td></tr>';
			backup = backups[++bIndex];
		    }
		    html += '</table>';
		    html += '</div>';
		    html += '</td>';
		}

		html += '</tr>';
	    }

	    me.setHtml(html);
	},

	initComponent: function() {
	    let me = this;

	    if (!me.store) {
		throw "no store specified";
	    }

	    let reload = function() {
		me.reload();
	    };

	    me.store.on("datachanged", reload);

	    me.callParent();

	    me.reload();
	},
    });

    Ext.define('PBS.PruneSimulatorPanel', {
	extend: 'Ext.panel.Panel',
	alias: 'widget.prunesimulatorPanel',

	viewModel: {
	    formulas: {
		calendarHidden: function(get) {
		    return !get('showCalendar.checked');
		},
	    },
	},

	getValues: function() {
	    let me = this;

	    let values = {};

	    Ext.Array.each(me.query('[isFormField]'), function(field) {
		let data = field.getSubmitData();
		Ext.Object.each(data, function(name, val) {
		    values[name] = val;
		});
	    });

	    return values;
	},

	controller: {
	    xclass: 'Ext.app.ViewController',

	    init: function(view) {
		this.reloadFull(); // initial load
	    },

	    control: {
		'field[fieldGroup=keep]': { change: 'reloadPrune' },
	    },

	    reloadFull: function() {
		let me = this;
		let view = me.getView();

		let params = view.getValues();

		let [hourSpec, minuteSpec] = params['schedule-time'].split(':');

		if (!hourSpec || !minuteSpec) {
		    Ext.Msg.alert('Error', 'Invalid schedule');
		    return;
		}

		let matchTimeSpec = function(timeSpec, rangeMin, rangeMax) {
		    let specValues = timeSpec.split(',');
		    let matches = {};

		    let assertValid = function(value) {
			let num = Number(value);
			if (isNaN(num)) {
			    throw value + " is not an integer";
			} else if (value < rangeMin || value > rangeMax) {
			    throw "number '" + value + "' is not in the range '" + rangeMin + ".." + rangeMax + "'";
			}
			return num;
		    };

		    specValues.forEach(function(value) {
			if (value.includes('..')) {
			    let [start, end] = value.split('..');
			    start = assertValid(start);
			    end = assertValid(end);
			    if (start > end) {
				throw "interval start is bigger then interval end '" + start + " > " + end + "'";
			    }
			    for (let i = start; i <= end; i++) {
				matches[i] = 1;
			    }
			} else if (value.includes('/')) {
			    let [start, step] = value.split('/');
			    start = assertValid(start);
			    step = assertValid(step);
			    for (let i = start; i <= rangeMax; i += step) {
				matches[i] = 1;
			    }
			} else if (value === '*') {
			    for (let i = rangeMin; i <= rangeMax; i++) {
				matches[i] = 1;
			    }
			} else {
			    value = assertValid(value);
			    matches[value] = 1;
			}
		    });

		    return Object.keys(matches);
		};

		let hours, minutes;

		try {
		    hours = matchTimeSpec(hourSpec, 0, 23);
		    minutes = matchTimeSpec(minuteSpec, 0, 59);
		} catch (err) {
		    Ext.Msg.alert('Error', err);
		}

		let backups = me.populateFromSchedule(
		    params['schedule-weekdays'],
		    hours,
		    minutes,
		    params.numberOfWeeks,
		);

		me.pruneSelect(backups, params);

		view.pruneStore.setData(backups);
	    },

	    reloadPrune: function() {
		let me = this;
		let view = me.getView();

		let params = view.getValues();

		let backups = [];
		view.pruneStore.getData().items.forEach(function(item) {
		    backups.push({
			backuptime: item.data.backuptime,
		    });
		});

		me.pruneSelect(backups, params);

		view.pruneStore.setData(backups);
	    },

	    // backups are sorted descending by date
	    populateFromSchedule: function(weekdays, hours, minutes, weekCount) {
		let weekdayFlags = [
		    weekdays.includes('sun'),
		    weekdays.includes('mon'),
		    weekdays.includes('tue'),
		    weekdays.includes('wed'),
		    weekdays.includes('thu'),
		    weekdays.includes('fri'),
		    weekdays.includes('sat'),
		];

		let todaysDate = new Date(NOW.getTime());

		let timesOnSingleDay = [];

		hours.forEach(function(hour) {
		    minutes.forEach(function(minute) {
			todaysDate.setHours(hour);
			todaysDate.setMinutes(minute);
			timesOnSingleDay.push(todaysDate.getTime());
		    });
		});

		// ordering here and iterating backwards through days
		// ensures that everything is ordered
		timesOnSingleDay.sort(function(a, b) {
		    return a < b;
		});

		let backups = [];

		for (let i = 0; i < 7 * weekCount; i++) {
		    let daysDate = Ext.Date.subtract(todaysDate, Ext.Date.DAY, i);
		    let weekday = parseInt(Ext.Date.format(daysDate, 'w'), 10);
		    if (weekdayFlags[weekday]) {
			timesOnSingleDay.forEach(function(time) {
			    backups.push({
				backuptime: Ext.Date.subtract(new Date(time), Ext.Date.DAY, i),
			    });
			});
		    }
		}

		return backups;
	    },

	    pruneMark: function(backups, keepCount, keepName, idFunc) {
		if (!keepCount) {
		    return;
		}

		let alreadyIncluded = {};
		let newlyIncluded = {};
		let newlyIncludedCount = 0;

		let finished = false;

		backups.forEach(function(backup) {
		    let mark = backup.mark;
		    let id = idFunc(backup);

		    if (finished || alreadyIncluded[id]) {
			return;
		    }

		    if (mark) {
			if (mark === 'keep') {
			    alreadyIncluded[id] = true;
			}
			return;
		    }

		    if (!newlyIncluded[id]) {
			if (newlyIncludedCount >= keepCount) {
			    finished = true;
			    return;
			}
			newlyIncluded[id] = true;
			newlyIncludedCount++;
			backup.mark = 'keep';
			backup.keepName = keepName;
		    } else {
			backup.mark = 'remove';
		    }
		});
	    },

	    // backups need to be sorted descending by date
	    pruneSelect: function(backups, keepParams) {
		let me = this;

		if (Number(keepParams['keep-last']) +
		    Number(keepParams['keep-hourly']) +
		    Number(keepParams['keep-daily']) +
		    Number(keepParams['keep-weekly']) +
		    Number(keepParams['keep-monthly']) +
		    Number(keepParams['keep-yearly']) === 0) {
		    backups.forEach(function(backup) {
			backup.mark = 'keep';
			backup.keepName = 'all zero';
		    });

		    return;
		}

		me.pruneMark(backups, keepParams['keep-last'], 'keep-last', function(backup) {
		    return backup.backuptime;
		});
		me.pruneMark(backups, keepParams['keep-hourly'], 'keep-hourly', function(backup) {
		    return Ext.Date.format(backup.backuptime, 'H/d/m/Y');
		});
		me.pruneMark(backups, keepParams['keep-daily'], 'keep-daily', function(backup) {
		    return Ext.Date.format(backup.backuptime, 'd/m/Y');
		});
		me.pruneMark(backups, keepParams['keep-weekly'], 'keep-weekly', function(backup) {
		    // ISO-8601 week and week-based year
		    return Ext.Date.format(backup.backuptime, 'W/o');
		});
		me.pruneMark(backups, keepParams['keep-monthly'], 'keep-monthly', function(backup) {
		    return Ext.Date.format(backup.backuptime, 'm/Y');
		});
		me.pruneMark(backups, keepParams['keep-yearly'], 'keep-yearly', function(backup) {
		    return Ext.Date.format(backup.backuptime, 'Y');
		});

		backups.forEach(function(backup) {
		    backup.mark = backup.mark || 'remove';
		});
	    },
	},

	keepItems: [
	    {
		xtype: 'numberfield',
		name: 'keep-last',
		allowBlank: true,
		fieldLabel: 'keep-last',
		minValue: 0,
		value: 4,
		fieldGroup: 'keep',
		padding: '0 0 0 10',
	    },
	    {
		xtype: 'numberfield',
		name: 'keep-hourly',
		allowBlank: true,
		fieldLabel: 'keep-hourly',
		minValue: 0,
		value: 0,
		fieldGroup: 'keep',
		padding: '0 0 0 10',
	    },
	    {
		xtype: 'numberfield',
		name: 'keep-daily',
		allowBlank: true,
		fieldLabel: 'keep-daily',
		minValue: 0,
		value: 5,
		fieldGroup: 'keep',
		padding: '0 0 0 10',
	    },
	    {
		xtype: 'numberfield',
		name: 'keep-weekly',
		allowBlank: true,
		fieldLabel: 'keep-weekly',
		minValue: 0,
		value: 2,
		fieldGroup: 'keep',
		padding: '0 0 0 10',
	    },
	    {
		xtype: 'numberfield',
		name: 'keep-monthly',
		allowBlank: true,
		fieldLabel: 'keep-monthly',
		minValue: 0,
		value: 0,
		fieldGroup: 'keep',
		padding: '0 0 0 10',
	    },
	    {
		xtype: 'numberfield',
		name: 'keep-yearly',
		allowBlank: true,
		fieldLabel: 'keep-yearly',
		minValue: 0,
		value: 0,
		fieldGroup: 'keep',
		padding: '0 0 0 10',
	    },
	],

	initComponent: function() {
	    var me = this;

	    me.pruneStore = Ext.create('Ext.data.Store', {
		model: 'pbs-prune-list',
		sorters: { property: 'backuptime', direction: 'DESC' },
	    });

	    let scheduleItems = [
		{
		    xtype: 'prunesimulatorDayOfWeekSelector',
		    name: 'schedule-weekdays',
		    fieldLabel: 'Day of week',
		    value: ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'],
		    allowBlank: false,
		    multiSelect: true,
		    padding: '0 0 0 10',
		},
		{
		    xtype: 'prunesimulatorCalendarEvent',
		    name: 'schedule-time',
		    allowBlank: false,
		    value: '0/6:00',
		    fieldLabel: 'Backup schedule',
		    padding: '0 0 0 10',
		},
		{
		    xtype: 'numberfield',
		    name: 'numberOfWeeks',
		    allowBlank: false,
		    fieldLabel: 'Number of weeks',
		    minValue: 1,
		    value: 15,
		    maxValue: 200,
		    padding: '0 0 0 10',
		},
		{
		    xtype: 'button',
		    name: 'schedule-button',
		    text: 'Update Schedule',
		    handler: function() {
			me.controller.reloadFull();
		    },
		},
	    ];

	    me.items = [
		{
		    xtype: 'panel',
		    layout: 'hbox',
		    height: 180,
		    items: [
			{
			    title: 'View',
			    layout: 'anchor',
			    flex: 1,
			    items: [
				{
				    padding: '0 0 0 10',
				    xtype: 'checkbox',
				    name: 'showCalendar',
				    reference: 'showCalendar',
				    fieldLabel: 'Show Calendar:',
				    checked: false,
				},
				{
				    padding: '0 0 0 10',
				    xtype: 'checkbox',
				    name: 'showColors',
				    reference: 'showColors',
				    fieldLabel: 'Show Colors:',
				    checked: false,
				    handler: function(checkbox, checked) {
					Ext.Array.each(me.query('[isFormField]'), function(field) {
					    if (field.fieldGroup !== 'keep') {
						return;
					    }

					    if (checked) {
						field.setFieldStyle('background-color: ' + COLORS[field.name] + '; ' +
						    'color: ' + TEXT_COLORS[field.name] + ';');
					    } else {
						field.setFieldStyle('background-color: white; color: black;');
					    }
					});

					me.lookupReference('weekTable').useColors = checked;
					me.lookupReference('pruneList').useColors = checked;

					me.controller.reloadPrune();
				    },
				},
			    ],
			},
			{
			    layout: 'anchor',
			    flex: 1,
			    title: 'Backup Schedule',
			    items: scheduleItems,
			},
		    ],
		},
		{
		    xtype: 'panel',
		    layout: 'hbox',
		    flex: 1,
		    items: [
			{
			    layout: 'anchor',
			    title: 'Prune Options',
			    items: me.keepItems,
			    flex: 1,
			},
			{
			    layout: 'fit',
			    title: 'Backups',
			    xtype: 'prunesimulatorPruneList',
			    store: me.pruneStore,
			    reference: 'pruneList',
			    height: '100%',
			    flex: 1,
			},
		    ],
		},
		{
		    layout: 'anchor',
		    title: 'Calendar',
		    autoScroll: true,
		    flex: 2,
		    xtype: 'prunesimulatorWeekTable',
		    reference: 'weekTable',
		    store: me.pruneStore,
		    bind: {
			hidden: '{calendarHidden}',
		    },
		},
	    ];

	    me.callParent();
	},
    });

    Ext.create('Ext.container.Viewport', {
	layout: 'border',
	renderTo: Ext.getBody(),
	items: [
	    {
		xtype: 'prunesimulatorPanel',
		title: 'PBS Prune Simulator',
		region: 'west',
		layout: {
		    type: 'vbox',
		    align: 'stretch',
		    pack: 'start',
		},
		width: 1080,
	    },
	    {
		xtype: 'prunesimulatorDocumentation',
		title: 'Usage',
		margins: '5 0 0 0',
		region: 'center',
	    },
	],
    });
});
