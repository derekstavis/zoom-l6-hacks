//! Live, read-only FreeRTOS diagnostics.
use crate::NativePanel;
use gpui::{Context, Entity, Render, Window, div, prelude::*};
use gpui_component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
    scroll::ScrollableElement,
    status_bar::StatusBar,
    tab::{Tab as KitTab, TabBar},
    table::{Table, TableBody, TableCell, TableHead, TableHeader, TableRow},
};
use l6max_host::freertos::{self, HeapSnapshot, SchedulerSnapshot, TasksSnapshot};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Heap,
    Tasks,
    Scheduler,
}
pub struct DebugWindow {
    panel: Entity<NativePanel>,
    tab: Tab,
    heap: Option<HeapSnapshot>,
    tasks: Option<TasksSnapshot>,
    scheduler: Option<SchedulerSnapshot>,
    error: Option<String>,
    refreshing: bool,
    connection: Option<l6max_host::qmp::QmpClient>,
}
impl DebugWindow {
    #[cfg(feature = "headless-screenshots")]
    pub fn preview(panel: Entity<NativePanel>, kind: &str) -> Self {
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
        let tasks_data = [
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
            state: if i == 1 { "Current" } else { "Waiting" }.into(),
            tcb: 0x808ad2d0 + i as u32 * 0x1100,
            priority: if i == 6 { 0 } else { 14 },
            base_priority: if i == 6 { 0 } else { 14 },
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
        Self {
            panel,
            tab: match kind {
                "debug-tasks" => Tab::Tasks,
                "debug-scheduler" => Tab::Scheduler,
                _ => Tab::Heap,
            },
            heap: Some(heap),
            tasks: Some(TasksSnapshot { tasks: tasks_data }),
            scheduler: Some(SchedulerSnapshot {
                task_count: 43,
                running: true,
                suspended: 0,
                ticks: 123456,
                pending_ticks: 0,
                current_tcb: 0x808ad2d0,
            }),
            error: None,
            refreshing: true,
            connection: None,
        }
    }
    pub fn new(panel: Entity<NativePanel>, cx: &mut Context<Self>) -> Self {
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
                                    view.heap = Some(heap);
                                    view.error = None;
                                }
                                Ok(Snapshot::Tasks(tasks)) => {
                                    view.tasks = Some(tasks);
                                    view.error = None;
                                }
                                Ok(Snapshot::Scheduler(scheduler)) => {
                                    view.scheduler = Some(scheduler);
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
}
fn bytes(value: u32) -> String {
    format!("{:.1} KiB", value as f64 / 1024.0)
}
fn head(label: &'static str, cx: &gpui::App) -> TableHead {
    TableHead::new()
        .text_color(cx.theme().muted_foreground)
        .child(label)
}
fn table_cell(text: impl Into<gpui::SharedString>) -> TableCell {
    TableCell::new().child(text.into())
}
impl Render for DebugWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    let mut rows = TableBody::new();
                    for b in &heap.blocks {
                        let state = if b.allocated { "Allocated" } else { "Free" };
                        let color = if b.allocated {
                            cx.theme().foreground
                        } else {
                            cx.theme().success
                        };
                        rows = rows.child(
                            TableRow::new()
                                .child(table_cell(format!("0x{:08x}", b.address)))
                                .child(table_cell(state).text_color(color))
                                .child(table_cell(bytes(b.size)))
                                .child(table_cell(b.owner.as_deref().unwrap_or("—").to_owned())),
                        );
                    }
                    let header = TableHeader::new().child(
                        TableRow::new()
                            .child(head("Block address", cx))
                            .child(head("State", cx))
                            .child(head("Block size", cx))
                            .child(head("Owner", cx)),
                    );
                    body = body.child(
                        div()
                            .id("heap-blocks")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .child(
                                Table::new()
                                    .small()
                                    .accessibility_label("Heap blocks")
                                    .child(header)
                                    .child(rows),
                            ),
                    );
                    format!(
                        "{} free · {} min · {} max block · {} total · {} alloc / {} frees",
                        bytes(c.free),
                        bytes(c.minimum_ever_free),
                        bytes(largest),
                        bytes(c.size),
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
                    let mut rows = TableBody::new();
                    for t in &tasks.tasks {
                        let state_color = match t.state.as_str() {
                            "Current" => cx.theme().success,
                            "Ready" => cx.theme().info,
                            _ => cx.theme().muted_foreground,
                        };
                        let stack_color = if t.stack_remaining.saturating_mul(10) < t.stack_size {
                            cx.theme().danger
                        } else if t.stack_remaining.saturating_mul(4) < t.stack_size {
                            cx.theme().warning
                        } else {
                            cx.theme().foreground
                        };
                        rows = rows.child(
                            TableRow::new()
                                .child(
                                    table_cell(format!(
                                        "{}{}",
                                        t.name,
                                        if t.current { "  •" } else { "" }
                                    ))
                                    .col_span(2),
                                )
                                .child(table_cell(t.state.clone()).text_color(state_color))
                                .child(table_cell(if t.priority == t.base_priority {
                                    t.priority.to_string()
                                } else {
                                    format!("{} / {}", t.priority, t.base_priority)
                                }))
                                .child(table_cell(bytes(t.stack_size)))
                                .child(table_cell(bytes(t.stack_remaining)).text_color(stack_color))
                                .child(table_cell(format!("0x{:08x}", t.stack_start))),
                        );
                    }
                    let header = TableHeader::new().child(
                        TableRow::new()
                            .child(head("Task", cx).col_span(2))
                            .child(head("State", cx))
                            .child(head("Priority", cx))
                            .child(head("Stack", cx))
                            .child(head("Free", cx))
                            .child(head("Stack base", cx)),
                    );
                    body = body.child(
                        div()
                            .id("tasks-table")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .child(
                                Table::new()
                                    .small()
                                    .accessibility_label("FreeRTOS tasks")
                                    .child(header)
                                    .child(rows),
                            ),
                    );
                    format!("{} tasks · stack free is an estimate", tasks.tasks.len())
                } else {
                    body = body.child("Waiting for a task sample…");
                    "Tasks".into()
                }
            }
            Tab::Scheduler => {
                if let Some(s) = &self.scheduler {
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
                    ];
                    let mut table_rows = TableBody::new();
                    for (label, value) in rows {
                        table_rows = table_rows.child(
                            TableRow::new()
                                .child(table_cell(label))
                                .child(table_cell(value)),
                        );
                    }
                    body = body.child(
                        Table::new()
                            .small()
                            .accessibility_label("FreeRTOS scheduler")
                            .child(table_rows),
                    );
                    format!("{} tasks · {} ticks", s.task_count, s.ticks)
                } else {
                    body = body.child("Waiting for a scheduler sample…");
                    "Scheduler".into()
                }
            }
        };
        let tabs = TabBar::new("freertos-tabs")
            .underline()
            .selected_index(match self.tab {
                Tab::Heap => 0,
                Tab::Tasks => 1,
                Tab::Scheduler => 2,
            })
            .child(KitTab::new().label("Heap"))
            .child(KitTab::new().label("Tasks"))
            .child(KitTab::new().label("Scheduler"))
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                this.tab = match index {
                    0 => Tab::Heap,
                    1 => Tab::Tasks,
                    _ => Tab::Scheduler,
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
                (Tab::Tasks | Tab::Scheduler, 0, _) => cx.theme().info,
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
