//! Stock L6 code fingerprints for the terminal DIN dispatch probe.
//!
//! These checks identify the specific routines used by the probe; they do not
//! identify an entire firmware image and do not by themselves authorize writes.
use crate::freertos::TasksSnapshot;
use crate::qmp::QmpClient;
use firmware_api::l6;
use l6fw::Model;
use sha2::{Digest, Sha256};
use std::time::Instant;

const SITES: [(u32, &str); 15] = [
    (
        l6::main_task,
        "12916ce507efb8853a8560185514653cb2f4384f85535a66be1fee3bc6379283",
    ),
    (
        l6::main_wait_call,
        "62be2a642037bc306a539c391e2cf78030eae697786402ad13248a023a6306a6",
    ),
    (
        l6::main_wait_return,
        "ddec7873e880ce1195f00e67e3273d2570cf8e62f5f490a2d7168edfa3cab5dc",
    ),
    (
        l6::main_state_return,
        "2391e25ca4be03b54698e41486ce19185062804f1b8d4f3c4cbeba227bce3458",
    ),
    (
        l6::main_wait_give,
        "04fd9245351da8bc1bf98bce7f281fbef63d37a01177e2cf7a5849bc68b8f33a",
    ),
    (
        l6::main_event_queue_reader,
        "8770796cd16904c61d3bfa2c3d767d5f91c6988e79fb9d705121f6f1a1521e6a",
    ),
    (
        l6::main_class2_event_handler,
        "29af02b30109fe144c19eedfd2acd8101d45bbddba95bee304382b13575e0748",
    ),
    (
        l6::idle_loop,
        "f4870ac4ce0c3cb238f92e437a9f5511912eee645168943b43a0166622b85ba2",
    ),
    (
        l6::main_event_submit,
        "aeac5eb8ff0f16e2d3ecc5d5cc41af4bc3334d08940d5fdb58cb267babf2744b",
    ),
    (
        l6::mixer_set_eq,
        "9cdb0f21e8f3a957d07e76e900b56439bb343b29907a9fcc4eab4a9546a4c8d7",
    ),
    (
        l6::mixer_din_setter_1,
        "86c0b4d3a801735dff30708ce6ad5f9f088cd8416ab7c9bf2746469d69bac578",
    ),
    (
        l6::mixer_din_setter_4,
        "eeaa5514e890165002016434ce3b1c9325c82469cac67115e09788ac60402030",
    ),
    (
        l6::mixer_din_setter_6,
        "d3ad8fd90e0be9dad58062764ff71b1fa57687853e9ea8cc0af79f73d8ae3ef9",
    ),
    (
        l6::mixer_din_setter_7,
        "c9db4effde92ebdbcb6c420c0990ffad38eff92e0e0df8b9fe40b93feb66eeae",
    ),
    (
        l6::mixer_din_setter_8,
        "467b9df9cd8d6ab8ddfd9dba2b09646df79a276ae3700d9eaa418fd6918d01b6",
    ),
];

/// Verify all code sites under the caller's deadline before starting the probe.
pub fn verify_before_checked(
    qmp: &QmpClient,
    model: Model,
    deadline: Instant,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    verify_with(model, check, |address| {
        qmp.read_memory_before(0, address, 64, deadline)
            .map_err(|e| e.to_string())
    })
}

fn verify_with(
    model: Model,
    check: &dyn Fn() -> Result<(), String>,
    mut read: impl FnMut(u32) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    if model != Model::L6 {
        return Err("DIN dispatch requires L6 1.10".into());
    }
    for (address, expected) in SITES {
        check()?;
        let bytes = read(address)?;
        check()?;
        if format!("{:x}", Sha256::digest(&bytes)) != expected {
            return Err(format!("DIN dispatch code mismatch at {address:#x}"));
        }
    }
    Ok(())
}

/// Report the current `Idle` task stack allocation and SP-based space. This does not
/// establish the stack depth needed by any synthetic call.
pub fn idle_stack_evidence(
    tasks: &TasksSnapshot,
    sp: u32,
    xpsr: u32,
) -> Result<(u32, u32, u32), String> {
    if xpsr & 0x1ff != 0 || xpsr & (1 << 24) == 0 {
        return Err("idle context is not Thumb thread mode".into());
    }
    if sp & 7 != 0 {
        return Err("idle stack pointer is not 8-byte aligned".into());
    }
    let mut current = tasks.tasks.iter().filter(|task| task.current);
    let task = current.next().ok_or("current task is missing")?;
    if current.next().is_some() || task.name != "Idle" {
        return Err("current task is not uniquely the L6 Idle task".into());
    }
    let end = task
        .stack_start
        .checked_add(task.stack_size)
        .ok_or("stack extent overflow")?;
    if sp < task.stack_start || sp.checked_add(4).is_none_or(|p| p > end) {
        return Err("idle stack argument is outside its allocation".into());
    }
    Ok((task.stack_start, end, sp - task.stack_start))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tasks() -> TasksSnapshot {
        TasksSnapshot {
            tasks: vec![crate::freertos::Task {
                name: "Idle".into(),
                state: "Current".into(),
                tcb: 1,
                priority: 0,
                base_priority: 0,
                creation_number: 0,
                mutexes_held: 0,
                event_list: None,
                stack_start: 0x80200000,
                stack_size: 1024,
                stack_remaining: 0,
                current: true,
            }],
        }
    }
    #[test]
    fn idle_stack_evidence_rejects_wrong_context_and_outside_stack() {
        let tasks = tasks();
        assert_eq!(
            idle_stack_evidence(&tasks, 0x80200300, 1 << 24).unwrap(),
            (0x80200000, 0x80200400, 768)
        );
        assert!(idle_stack_evidence(&tasks, 0x80200400, 1 << 24).is_err());
        assert!(idle_stack_evidence(&tasks, 0x80200304, 1 << 24).is_err());
        assert!(idle_stack_evidence(&tasks, 0x80200300, (1 << 24) | 1).is_err());
        assert!(idle_stack_evidence(&tasks, 0x80200300, 0).is_err());
        let mut other = tasks;
        other.tasks[0].name = "Main".into();
        assert!(idle_stack_evidence(&other, 0x80200300, 1 << 24).is_err());
    }
    #[test]
    fn guard_rejects_wrong_model_and_code_before_continuing() {
        let mut reads = 0;
        assert!(
            verify_with(Model::L6max, &|| Ok(()), |_| {
                reads += 1;
                Ok(vec![0; 64])
            })
            .is_err()
        );
        assert_eq!(reads, 0);
        assert!(
            verify_with(Model::L6, &|| Ok(()), |_| {
                reads += 1;
                Ok(vec![0; 64])
            })
            .unwrap_err()
            .contains("code mismatch")
        );
        assert_eq!(reads, 1);
    }

    #[test]
    fn guard_checks_cancellation_after_each_read() {
        let calls = std::cell::Cell::new(0);
        let check = || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                Err("cancelled".into())
            } else {
                Ok(())
            }
        };
        assert_eq!(
            verify_with(Model::L6, &check, |_| Ok(vec![0; 64])).unwrap_err(),
            "cancelled"
        );
    }
    #[test]
    #[ignore = "requires L6_DIN_DISPATCH_STOCK_MAIN"]
    fn all_guard_sites_match_stock_image() {
        let bytes = std::fs::read(std::env::var("L6_DIN_DISPATCH_STOCK_MAIN").unwrap()).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "827c7c5cb2a43cc3a011d4556462b8cb9539242f817ebeed7727d66a36c03799"
        );
        for (address, expected) in SITES {
            let start = (address - Model::L6.main_base()) as usize;
            assert_eq!(
                format!("{:x}", Sha256::digest(&bytes[start..start + 64])),
                expected,
                "{address:#x}"
            );
        }
    }
}
