//! Live, read-only FreeRTOS diagnostics.
use crate::NativePanel;
use gpui::{Context, Entity, Render, Window, div, prelude::*};
use gpui_component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
    status_bar::StatusBar,
    tab::{Tab as KitTab, TabBar},
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
};
use l6max_host::freertos::{self, HeapSnapshot, SchedulerSnapshot, TasksSnapshot};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Heap,
    Tasks,
    Scheduler,
    Objects,
}
pub struct DebugWindow {
    panel: Entity<NativePanel>,
    tab: Tab,
    heap: Option<HeapSnapshot>,
    tasks: Option<TasksSnapshot>,
    scheduler: Option<SchedulerSnapshot>,
    objects: Option<Vec<freertos::Object>>,
    error: Option<String>,
    refreshing: bool,
    connection: Option<l6max_host::qmp::QmpClient>,
    heap_table: Entity<TableState<DebugTable>>,
    tasks_table: Entity<TableState<DebugTable>>,
    scheduler_table: Entity<TableState<DebugTable>>,
    objects_table: Entity<TableState<DebugTable>>,
}
impl DebugWindow {
    #[cfg(feature = "headless-screenshots")]
    pub fn preview(
        panel: Entity<NativePanel>,
        kind: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        use l6max_host::freertos::{HeapBlock, Task};
        let heap = HeapSnapshot {
            counters: l6max_host::heap::Heap {
                size: 511992,
                free: 124448,
                minimum_ever_free: 120000,
                allocations: 237,
                frees: 0,
            },
            blocks: vec![
                HeapBlock {
                    address: 0x808a4210,
                    size: 4096,
                    allocated: true,
                    owner: Some("ApTimer · stack".into()),
                },
                HeapBlock {
                    address: 0x808a5210,
                    size: 160,
                    allocated: true,
                    owner: Some("ApTimer · task".into()),
                },
                HeapBlock {
                    address: 0x808a52b0,
                    size: 32784,
                    allocated: true,
                    owner: Some("Main · stack".into()),
                },
                HeapBlock {
                    address: 0x808f2b20,
                    size: 36880,
                    allocated: false,
                    owner: None,
                },
                HeapBlock {
                    address: 0x8090bb30,
                    size: 87568,
                    allocated: false,
                    owner: None,
                },
            ],
        };
        let tasks_data: Vec<freertos::Task> = [
            "Main",
            "AudioProcess",
            "AudioEfxQFunc",
            "UsbPollingTask",
            "DisplayCycle1",
            "RecPlayCtl",
            "IDLE",
        ]
        .iter()
        .enumerate()
        .map(|(i, name)| Task {
            name: (*name).into(),
            state: if i == 1 { "Current" } else { "Blocked" }.into(),
            tcb: 0x808ad2d0 + i as u32 * 0x1100,
            priority: if i == 6 { 0 } else { 14 },
            base_priority: if i == 6 { 0 } else { 14 },
            creation_number: i as u32 + 1,
            mutexes_held: 0,
            event_list: None,
            stack_start: 0x808a52c0 + i as u32 * 0x1000,
            stack_size: if i == 0 { 32768 } else { 4096 },
            stack_remaining: match i {
                0 => 24812,
                2 => 700,
                3 => 360,
                _ => 3296,
            },
            current: i == 1,
        })
        .collect();
        let heap_table = new_table(DebugRows::Heap(heap.blocks.clone()), window, cx);
        let tasks_table = new_table(DebugRows::Tasks(tasks_data.clone()), window, cx);
        let scheduler = SchedulerSnapshot {
            task_count: 43,
            running: true,
            suspended: 0,
            ticks: 123456,
            pending_ticks: 0,
            current_tcb: 0x808ad2d0,
            yield_pending: false,
            tick_overflows: 0,
            next_wake: Some(123500),
            critical_nesting: 0,
        };
        let scheduler_table = new_table(DebugRows::Scheduler(scheduler.clone()), window, cx);
        let objects = vec![freertos::Object {
            name: "Notification semaphore",
            address: 0x808cb608,
            count: 1,
            capacity: 1,
            send_waiters: 0,
            receive_waiters: 0,
        }];
        let objects_table = new_table(DebugRows::Objects(objects.clone()), window, cx);
        Self {
            panel,
            heap_table,
            tasks_table,
            scheduler_table,
            objects_table,
            objects: Some(objects),
            tab: match kind {
                "debug-tasks" => Tab::Tasks,
                "debug-scheduler" => Tab::Scheduler,
                "debug-objects" => Tab::Objects,
                _ => Tab::Heap,
            },
            heap: Some(heap),
            tasks: Some(TasksSnapshot { tasks: tasks_data }),
            scheduler: Some(scheduler),
            error: None,
            refreshing: true,
            connection: None,
        }
    }
    pub fn new(panel: Entity<NativePanel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let heap_table = new_table(DebugRows::Heap(Vec::new()), window, cx);
        let tasks_table = new_table(DebugRows::Tasks(Vec::new()), window, cx);
        let scheduler_table = new_table(
            DebugRows::Scheduler(SchedulerSnapshot {
                task_count: 0,
                running: false,
                suspended: 0,
                ticks: 0,
                pending_ticks: 0,
                current_tcb: 0,
                yield_pending: false,
                tick_overflows: 0,
                next_wake: None,
                critical_nesting: 0,
            }),
            window,
            cx,
        );
        let objects_table = new_table(DebugRows::Objects(Vec::new()), window, cx);
        cx.spawn(async move |this, cx| {
            loop {
                let selected = this.update(cx, |view, cx| {
                    let panel = view.panel.read(cx);
                    let model = panel.model;
                    let qmp = panel
                        ._qemu
                        .as_ref()
                        .and_then(|engine| engine.management.first())
                        .cloned();
                    let same = match (&view.connection, &qmp) {
                        (Some(old), Some(current)) => old.same_connection(current),
                        (None, None) => true,
                        _ => false,
                    };
                    if !same {
                        view.heap = None;
                        view.tasks = None;
                        view.scheduler = None;
                        view.objects = None;
                        view.error = None;
                        view.connection = qmp.clone();
                        cx.notify();
                    }
                    (view.tab, view.refreshing, model, qmp)
                });
                let Ok((tab, refreshing, model, qmp)) = selected else {
                    break;
                };
                if !refreshing {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    continue;
                }
                let sampled_qmp = qmp.clone();
                let result = if let Some(qmp) = qmp {
                    cx.background_spawn(async move {
                        match tab {
                            Tab::Heap => freertos::heap(&qmp, model).map(Snapshot::Heap),
                            Tab::Tasks => freertos::tasks(&qmp, model).map(Snapshot::Tasks),
                            Tab::Scheduler => {
                                freertos::scheduler(&qmp, model).map(Snapshot::Scheduler)
                            }
                            Tab::Objects => freertos::objects(&qmp, model).map(Snapshot::Objects),
                        }
                    })
                    .await
                } else {
                    Err("Start the emulator to view FreeRTOS diagnostics.".into())
                };
                if this
                    .update(cx, |view, cx| {
                        let current_qmp = view
                            .panel
                            .read(cx)
                            ._qemu
                            .as_ref()
                            .and_then(|engine| engine.management.first());
                        let same_guest = match (&sampled_qmp, current_qmp) {
                            (Some(old), Some(current)) => old.same_connection(current),
                            (None, None) => true,
                            _ => false,
                        };
                        if view.tab == tab && view.refreshing && same_guest {
                            match result {
                                Ok(Snapshot::Heap(heap)) => {
                                    view.heap_table.update(cx, |table, cx| {
                                        table
                                            .delegate_mut()
                                            .set_rows(DebugRows::Heap(heap.blocks.clone()));
                                        cx.notify();
                                    });
                                    view.heap = Some(heap);
                                    view.error = None;
                                }
                                Ok(Snapshot::Tasks(tasks)) => {
                                    view.tasks_table.update(cx, |table, cx| {
                                        table
                                            .delegate_mut()
                                            .set_rows(DebugRows::Tasks(tasks.tasks.clone()));
                                        cx.notify();
                                    });
                                    view.tasks = Some(tasks);
                                    view.error = None;
                                }
                                Ok(Snapshot::Scheduler(scheduler)) => {
                                    view.scheduler_table.update(cx, |table, cx| {
                                        table
                                            .delegate_mut()
                                            .set_rows(DebugRows::Scheduler(scheduler.clone()));
                                        cx.notify();
                                    });
                                    view.scheduler = Some(scheduler);
                                    view.error = None;
                                }
                                Ok(Snapshot::Objects(objects)) => {
                                    view.objects_table.update(cx, |table, cx| {
                                        table
                                            .delegate_mut()
                                            .set_rows(DebugRows::Objects(objects.clone()));
                                        cx.notify();
                                    });
                                    view.objects = Some(objects);
                                    view.error = None;
                                }
                                Err(error) => view.error = Some(error),
                            }
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        })
        .detach();
        Self {
            panel,
            tab: Tab::Heap,
            heap_table,
            tasks_table,
            scheduler_table,
            objects_table,
            objects: None,
            heap: None,
            tasks: None,
            scheduler: None,
            error: None,
            refreshing: true,
            connection: None,
        }
    }
}
enum Snapshot {
    Heap(HeapSnapshot),
    Tasks(TasksSnapshot),
    Scheduler(SchedulerSnapshot),
    Objects(Vec<freertos::Object>),
}
fn bytes(value: u32) -> String {
    format!("{:.1} KiB", value as f64 / 1024.0)
}
#[derive(Clone)]
enum DebugRows {
    Heap(Vec<freertos::HeapBlock>),
    Tasks(Vec<freertos::Task>),
    Scheduler(SchedulerSnapshot),
    Objects(Vec<freertos::Object>),
}
struct DebugTable {
    rows: DebugRows,
    sort: Option<(usize, ColumnSort)>,
    width: f32,
}
impl DebugTable {
    fn numeric_column(&self, col: usize) -> bool {
        match self.rows {
            DebugRows::Heap(_) => col >= 2,
            DebugRows::Tasks(_) => col >= 2,
            DebugRows::Scheduler(_) => col == 1,
            DebugRows::Objects(_) => col >= 1,
        }
    }
    fn set_rows(&mut self, rows: DebugRows) {
        self.rows = rows;
        self.apply_sort();
    }
    fn apply_sort(&mut self) {
        let sort = self.sort;
        match &mut self.rows {
            DebugRows::Heap(rows) => rows.sort_by(|a, b| {
                let (column, direction) = sort.unwrap_or((3, ColumnSort::Ascending));
                let order = match column {
                    0 => a.allocated.cmp(&b.allocated),
                    1 => a.owner.cmp(&b.owner),
                    2 => a.size.cmp(&b.size),
                    _ => a.address.cmp(&b.address),
                };
                (if direction == ColumnSort::Descending {
                    order.reverse()
                } else {
                    order
                })
                .then(a.address.cmp(&b.address))
            }),
            DebugRows::Tasks(rows) => rows.sort_by(|a, b| {
                let (column, direction) = sort.unwrap_or((2, ColumnSort::Descending));
                let order = match column {
                    0 => a.name.cmp(&b.name),
                    1 => a.state.cmp(&b.state),
                    2 => a.priority.cmp(&b.priority),
                    3 => a.stack_size.cmp(&b.stack_size),
                    4 => a.stack_remaining.cmp(&b.stack_remaining),
                    5 => (a.stack_remaining as u64 * b.stack_size as u64)
                        .cmp(&(b.stack_remaining as u64 * a.stack_size as u64)),
                    _ => a.stack_start.cmp(&b.stack_start),
                };
                (if direction == ColumnSort::Descending {
                    order.reverse()
                } else {
                    order
                })
                .then(a.name.cmp(&b.name))
                .then(a.tcb.cmp(&b.tcb))
            }),
            DebugRows::Scheduler(_) => {}
            DebugRows::Objects(rows) => rows.sort_by(|a, b| {
                let (col, direction) = sort.unwrap_or((0, ColumnSort::Ascending));
                let order = match col {
                    1 => a.count.cmp(&b.count),
                    2 => a.capacity.cmp(&b.capacity),
                    3 => a.send_waiters.cmp(&b.send_waiters),
                    4 => a.receive_waiters.cmp(&b.receive_waiters),
                    5 => a.address.cmp(&b.address),
                    _ => a.name.cmp(b.name),
                };
                (if direction == ColumnSort::Descending {
                    order.reverse()
                } else {
                    order
                })
                .then(a.address.cmp(&b.address))
            }),
        }
    }
    fn labels(&self) -> &'static [&'static str] {
        match self.rows {
            DebugRows::Heap(_) => &["State", "Owner", "Block size", "Block address"],
            DebugRows::Tasks(_) => &[
                "Task",
                "State",
                "Priority",
                "Stack",
                "Free",
                "Free %",
                "Stack base",
            ],
            DebugRows::Objects(_) => &[
                "Object",
                "Count",
                "Capacity",
                "Send wait",
                "Recv wait",
                "Address",
            ],
            DebugRows::Scheduler(_) => &["Property", "Value"],
        }
    }
    fn cell(&self, row: usize, col: usize) -> String {
        match &self.rows {
            DebugRows::Heap(rows) => {
                let b = &rows[row];
                match col {
                    0 => if b.allocated { "Allocated" } else { "Free" }.into(),
                    1 => b
                        .owner
                        .as_deref()
                        .unwrap_or(if b.allocated { "Unknown" } else { "—" })
                        .into(),
                    2 => bytes(b.size),
                    _ => format!("0x{:08x}", b.address),
                }
            }
            DebugRows::Tasks(rows) => {
                let t = &rows[row];
                match col {
                    0 => format!("{}{}", t.name, if t.current { "  •" } else { "" }),
                    1 => t.state.clone(),
                    2 => {
                        if t.priority == t.base_priority {
                            t.priority.to_string()
                        } else {
                            format!("{} / {}", t.priority, t.base_priority)
                        }
                    }
                    3 => bytes(t.stack_size),
                    4 => bytes(t.stack_remaining),
                    5 => format!(
                        "{}%",
                        (t.stack_remaining as u64 * 100) / (t.stack_size as u64).max(1)
                    ),
                    _ => format!("0x{:08x}", t.stack_start),
                }
            }
            DebugRows::Objects(rows) => {
                let o = &rows[row];
                match col {
                    0 => o.name.into(),
                    1 => o.count.to_string(),
                    2 => o.capacity.to_string(),
                    3 => o.send_waiters.to_string(),
                    4 => o.receive_waiters.to_string(),
                    _ => format!("0x{:08x}", o.address),
                }
            }
            DebugRows::Scheduler(s) => {
                let rows = [
                    (
                        "State",
                        if s.running {
                            "Running".into()
                        } else {
                            "Not started".into()
                        },
                    ),
                    ("Tasks", s.task_count.to_string()),
                    ("Ticks", s.ticks.to_string()),
                    ("Pending ticks", s.pending_ticks.to_string()),
                    ("Suspension depth", s.suspended.to_string()),
                    ("Current TCB", format!("0x{:08x}", s.current_tcb)),
                    (
                        "Yield pending",
                        if s.yield_pending {
                            "Yes".into()
                        } else {
                            "No".into()
                        },
                    ),
                    ("Tick overflows", s.tick_overflows.to_string()),
                    (
                        "Next wake tick",
                        s.next_wake.map_or("None".into(), |v| v.to_string()),
                    ),
                    ("Critical nesting", s.critical_nesting.to_string()),
                ];
                if col == 0 {
                    rows[row].0.into()
                } else {
                    rows[row].1.clone()
                }
            }
        }
    }
}
fn new_table(
    rows: DebugRows,
    window: &mut Window,
    cx: &mut Context<DebugWindow>,
) -> Entity<TableState<DebugTable>> {
    let mut delegate = DebugTable {
        rows,
        sort: None,
        width: (f32::from(window.viewport_size().width) - 32.0).max(0.0),
    };
    delegate.apply_sort();
    cx.new(|cx| {
        TableState::new(delegate, window, cx)
            .col_movable(false)
            .col_resizable(false)
            .row_selectable(false)
    })
}
impl TableDelegate for DebugTable {
    fn columns_count(&self, _: &gpui::App) -> usize {
        self.labels().len()
    }
    fn rows_count(&self, _: &gpui::App) -> usize {
        match &self.rows {
            DebugRows::Heap(r) => r.len(),
            DebugRows::Tasks(r) => r.len(),
            DebugRows::Scheduler(_) => 10,
            DebugRows::Objects(r) => r.len(),
        }
    }
    fn column(&self, col: usize, _: &gpui::App) -> Column {
        let weight = match self.rows {
            DebugRows::Tasks(_) if col == 0 => 1.8,
            DebugRows::Tasks(_) if col == 6 => 2.0,
            DebugRows::Objects(_) if col == 0 => 2.2,
            DebugRows::Objects(_) if col == 5 => 1.5,
            _ => 1.,
        };
        let count = self.labels().len() as f32
            + match self.rows {
                DebugRows::Tasks(_) => 1.8,
                DebugRows::Objects(_) => 1.7,
                _ => 0.,
            };
        let column = Column::new(format!("column-{col}"), self.labels()[col])
            .width(gpui::px((self.width / count * weight).max(70.)));
        let column = if self.numeric_column(col) {
            column.text_right()
        } else {
            column
        };
        if matches!(self.rows, DebugRows::Scheduler(_)) {
            column
        } else {
            column.sort(
                if let Some((i, sort)) = self.sort
                    && i == col
                {
                    sort
                } else {
                    ColumnSort::Default
                },
            )
        }
    }
    fn perform_sort(
        &mut self,
        col: usize,
        sort: ColumnSort,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        self.sort = if sort == ColumnSort::Default {
            None
        } else {
            Some((col, sort))
        };
        self.apply_sort();
    }
    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if let DebugRows::Tasks(rows) = &self.rows
            && col == 0
        {
            let task = &rows[row];
            let details = format!(
                "TCB: 0x{:08x} · creation: {} · mutexes held: {} · event list: {}",
                task.tcb,
                task.creation_number,
                task.mutexes_held,
                task.event_list
                    .map_or("unknown".into(), |v| format!("0x{v:08x}"))
            );
            return div()
                .id(("task-details", row))
                .text_color(cx.theme().foreground)
                .child(self.cell(row, col))
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(details.clone()).build(window, cx)
                })
                .into_any_element();
        }
        let color = match &self.rows {
            DebugRows::Heap(rows) if col == 0 && !rows[row].allocated => cx.theme().success,
            DebugRows::Tasks(rows) if col == 1 => match rows[row].state.as_str() {
                "Current" => cx.theme().success,
                "Ready" => cx.theme().info,
                _ => cx.theme().muted_foreground,
            },
            DebugRows::Tasks(rows)
                if (col == 4 || col == 5)
                    && rows[row].stack_remaining.saturating_mul(10) < rows[row].stack_size =>
            {
                cx.theme().danger
            }
            DebugRows::Tasks(rows)
                if (col == 4 || col == 5)
                    && rows[row].stack_remaining.saturating_mul(4) < rows[row].stack_size =>
            {
                cx.theme().warning
            }
            _ => cx.theme().foreground,
        };
        let cell = div().text_color(color).child(self.cell(row, col));
        if self.numeric_column(col) {
            if matches!(self.rows, DebugRows::Scheduler(_)) && row == 0 {
                cell.into_any_element()
            } else {
                cell.font_family(cx.theme().mono_font_family.clone())
                    .into_any_element()
            }
        } else {
            cell.into_any_element()
        }
    }
    fn cell_label(&self, row: usize, col: usize, _: &gpui::App) -> Option<gpui::SharedString> {
        if let DebugRows::Tasks(rows) = &self.rows
            && col == 0
        {
            let task = &rows[row];
            return Some(
                format!(
                    "{}, TCB 0x{:08x}, creation {}, mutexes held {}, event list {}",
                    self.cell(row, col),
                    task.tcb,
                    task.creation_number,
                    task.mutexes_held,
                    task.event_list
                        .map_or("unknown".into(), |v| format!("0x{v:08x}"))
                )
                .into(),
            );
        }
        Some(self.cell(row, col).into())
    }
}
impl Render for DebugWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = (f32::from(window.viewport_size().width) - 32.0).max(0.0);
        for table in [
            &self.heap_table,
            &self.tasks_table,
            &self.scheduler_table,
            &self.objects_table,
        ] {
            if (table.read(cx).delegate().width - width).abs() > 0.5 {
                table.update(cx, |table, cx| {
                    table.delegate_mut().width = width;
                    table.refresh(cx);
                });
            }
        }
        let mut body = div().flex_1().min_h_0().flex().flex_col().gap_3();
        if let Some(error) = &self.error {
            body = body.child(gpui_component::alert::Alert::error(
                "debug-error",
                error.clone(),
            ));
        }
        let summary = match self.tab {
            Tab::Heap => {
                if let Some(heap) = &self.heap {
                    let c = heap.counters;
                    let largest = heap
                        .blocks
                        .iter()
                        .filter(|b| !b.allocated)
                        .map(|b| b.size)
                        .max()
                        .unwrap_or(0);
                    body = body.child(
                        div().flex_1().min_h_0().child(
                            DataTable::new(&self.heap_table)
                                .small()
                                .bordered(false)
                                .accessibility_label("Heap blocks"),
                        ),
                    );
                    let free_blocks = heap.blocks.iter().filter(|b| !b.allocated).count();
                    let fragmentation = if c.free == 0 {
                        "—".to_owned()
                    } else {
                        format!("{}%", (c.free - largest) as u64 * 100 / c.free as u64)
                    };
                    format!(
                        "{} free · {} min · {} max block · {} free block{} · {} fragmented · {} alloc / {} frees",
                        bytes(c.free),
                        bytes(c.minimum_ever_free),
                        bytes(largest),
                        free_blocks,
                        if free_blocks == 1 { "" } else { "s" },
                        fragmentation,
                        c.allocations,
                        c.frees
                    )
                } else {
                    body = body.child("Waiting for a heap sample…");
                    "Heap".into()
                }
            }
            Tab::Tasks => {
                if let Some(tasks) = &self.tasks {
                    body = body.child(
                        div().flex_1().min_h_0().child(
                            DataTable::new(&self.tasks_table)
                                .small()
                                .bordered(false)
                                .accessibility_label("FreeRTOS tasks"),
                        ),
                    );
                    let lowest = tasks.tasks.iter().min_by_key(|t| {
                        (t.stack_remaining as u64 * 10000) / (t.stack_size as u64).max(1)
                    });
                    lowest.map_or_else(
                        || "0 tasks".into(),
                        |t| {
                            format!(
                                "{} tasks · lowest: {} {}% · stack free is an estimate",
                                tasks.tasks.len(),
                                t.name,
                                (t.stack_remaining as u64 * 100) / (t.stack_size as u64).max(1)
                            )
                        },
                    )
                } else {
                    body = body.child("Waiting for a task sample…");
                    "Tasks".into()
                }
            }
            Tab::Scheduler => {
                if let Some(s) = &self.scheduler {
                    body = body.child(
                        div().flex_1().min_h_0().child(
                            DataTable::new(&self.scheduler_table)
                                .small()
                                .bordered(false)
                                .accessibility_label("FreeRTOS scheduler"),
                        ),
                    );
                    format!("{} tasks · {} ticks", s.task_count, s.ticks)
                } else {
                    body = body.child("Waiting for a scheduler sample…");
                    "Scheduler".into()
                }
            }
            Tab::Objects => {
                if let Some(objects) = &self.objects {
                    body = body.child(
                        div().flex_1().min_h_0().child(
                            DataTable::new(&self.objects_table)
                                .small()
                                .bordered(false)
                                .accessibility_label("Known FreeRTOS objects"),
                        ),
                    );
                    format!(
                        "{} known object{} · partial list",
                        objects.len(),
                        if objects.len() == 1 { "" } else { "s" }
                    )
                } else {
                    body = body.child("Waiting for an object sample…");
                    "Objects".into()
                }
            }
        };
        let tabs = TabBar::new("freertos-tabs")
            .underline()
            .selected_index(match self.tab {
                Tab::Heap => 0,
                Tab::Tasks => 1,
                Tab::Scheduler => 2,
                Tab::Objects => 3,
            })
            .child(KitTab::new().label("Heap"))
            .child(KitTab::new().label("Tasks"))
            .child(KitTab::new().label("Scheduler"))
            .child(KitTab::new().label("Objects"))
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                this.tab = match index {
                    0 => Tab::Heap,
                    1 => Tab::Tasks,
                    2 => Tab::Scheduler,
                    _ => Tab::Objects,
                };
                this.error = None;
                cx.notify();
            }));
        let mut summary_content = div().flex().items_center().gap_2();
        for (index, part) in summary.split(" · ").enumerate() {
            if index > 0 {
                summary_content =
                    summary_content.child(div().text_color(cx.theme().muted_foreground).child("·"));
            }
            let color = match (self.tab, index, &self.heap) {
                (Tab::Heap, 0, Some(heap))
                    if heap.counters.free.saturating_mul(10) < heap.counters.size =>
                {
                    cx.theme().danger
                }
                (Tab::Heap, 0, Some(heap))
                    if heap.counters.free.saturating_mul(4) < heap.counters.size =>
                {
                    cx.theme().warning
                }
                (Tab::Heap, 0, Some(_)) => cx.theme().success,
                (Tab::Heap, 2, Some(_)) => cx.theme().info,
                (Tab::Tasks | Tab::Scheduler | Tab::Objects, 0, _) => cx.theme().info,
                _ => cx.theme().muted_foreground,
            };
            summary_content = summary_content.child(div().text_color(color).child(part.to_owned()));
        }
        let toggle = Button::new("refresh-toggle")
            .xsmall()
            .icon(gpui_component::Icon::new(if self.refreshing {
                gpui_kit_assets::IconName::Pause
            } else {
                gpui_kit_assets::IconName::Play
            }))
            .ghost()
            .tooltip(if self.refreshing {
                "Pause refresh"
            } else {
                "Resume refresh"
            })
            .accessibility_label(if self.refreshing {
                "Pause refresh"
            } else {
                "Resume refresh"
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.refreshing = !this.refreshing;
                cx.notify();
            }));
        div()
            .size_full()
            .flex()
            .flex_col()
            .p_4()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(div().flex_shrink_0().child(tabs))
            .child(body)
            .child(StatusBar::new().left(summary_content).right(toggle))
    }
}

#[cfg(test)]
mod sort_tests {
    use super::*;
    #[test]
    fn heap_sort_survives_new_snapshots_and_uses_numeric_sizes() {
        let blocks = || {
            vec![
                freertos::HeapBlock {
                    address: 8,
                    size: 128,
                    allocated: true,
                    owner: None,
                },
                freertos::HeapBlock {
                    address: 16,
                    size: 2048,
                    allocated: false,
                    owner: None,
                },
            ]
        };
        let mut table = DebugTable {
            rows: DebugRows::Heap(blocks()),
            sort: Some((2, ColumnSort::Descending)),
            width: 760.0,
        };
        table.set_rows(DebugRows::Heap(blocks()));
        assert_eq!(table.cell(0, 2), "2.0 KiB");
        table.set_rows(DebugRows::Heap(blocks()));
        assert_eq!(table.cell(0, 2), "2.0 KiB");
        table.sort = Some((2, ColumnSort::Ascending));
        table.apply_sort();
        assert_eq!(table.cell(0, 2), "0.1 KiB");
    }
    #[test]
    fn free_percentage_sorts_by_ratio_not_byte_count() {
        let task = |name: &str, stack_size, stack_remaining| freertos::Task {
            name: name.into(),
            state: "Blocked".into(),
            tcb: 0,
            priority: 1,
            base_priority: 1,
            creation_number: 1,
            mutexes_held: 0,
            event_list: None,
            stack_start: 0,
            stack_size,
            stack_remaining,
            current: false,
        };
        let mut table = DebugTable {
            rows: DebugRows::Tasks(vec![task("large", 1000, 100), task("small", 100, 20)]),
            sort: Some((5, ColumnSort::Ascending)),
            width: 760.,
        };
        table.apply_sort();
        assert_eq!(table.cell(0, 0), "large");
        assert_eq!(table.cell(0, 5), "10%");
        assert_eq!(table.cell(1, 5), "20%");
    }
}
