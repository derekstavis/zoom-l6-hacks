//! Read-only, revision-guarded FreeRTOS diagnostics for L6max 1.10.
use crate::{heap, qmp::QmpClient};
use l6fw::Model;
use sha2::{Digest, Sha256};

const START: u32 = 0x808a4148;
const END: u32 = 0x80921140;
const TASK_COUNT: u32 = 0x8020ab40;
const CURRENT_TCB: u32 = 0x808a3af0;
const CREATE_TASK: u32 = 0x80091360;
const CREATE_HASH: &str = "05215841341ae4fb9ffbbfb43250cea8f3149427f4ba5abd048cfb4dd5d22ec9";

#[derive(Clone, Debug)]
pub struct HeapBlock {
    pub address: u32,
    pub size: u32,
    pub allocated: bool,
    pub owner: Option<String>,
}
#[derive(Clone, Debug)]
pub struct HeapSnapshot {
    pub counters: heap::Heap,
    pub blocks: Vec<HeapBlock>,
}
#[derive(Clone, Debug)]
pub struct Task {
    pub name: String,
    pub state: String,
    pub tcb: u32,
    pub priority: u32,
    pub base_priority: u32,
    pub stack_start: u32,
    pub stack_size: u32,
    pub stack_remaining: u32,
    pub current: bool,
}
#[derive(Clone, Debug)]
pub struct SchedulerSnapshot {
    pub task_count: u32,
    pub running: bool,
    pub suspended: u32,
    pub ticks: u32,
    pub pending_ticks: u32,
    pub current_tcb: u32,
}
pub fn scheduler(q: &QmpClient, model: Model) -> Result<SchedulerSnapshot, String> {
    guard(q, model)?;
    if format!("{:x}", Sha256::digest(read(q, CREATE_TASK, 64)?)) != CREATE_HASH {
        return Err("Scheduler layout is unsupported for this firmware".into());
    }
    let b = read(q, TASK_COUNT, 0x28)?;
    let task_count = u32_at(&b, 0)?;
    if task_count > 256 {
        return Err("scheduler is not ready".into());
    }
    Ok(SchedulerSnapshot {
        task_count,
        running: u32_at(&b, 4)? != 0,
        suspended: u32_at(&b, 8)?,
        ticks: u32_at(&b, 12)?,
        pending_ticks: u32_at(&b, 0x24)?,
        current_tcb: u32_at(&read(q, CURRENT_TCB, 4)?, 0)?,
    })
}
#[derive(Clone, Debug)]
pub struct TasksSnapshot {
    pub tasks: Vec<Task>,
}
fn u32_at(b: &[u8], offset: usize) -> Result<u32, String> {
    let b = b.get(offset..offset + 4).ok_or("short memory read")?;
    Ok(u32::from_le_bytes(b.try_into().unwrap()))
}
fn read(q: &QmpClient, addr: u32, len: usize) -> Result<Vec<u8>, String> {
    q.read_memory(0, addr, len).map_err(|e| e.to_string())
}
fn guard(q: &QmpClient, model: Model) -> Result<heap::Heap, String> {
    if model != Model::L6max {
        return Err("FreeRTOS details are supported for L6max 1.10 only".into());
    }
    heap::read(q, model)
}
fn read_arena(q: &QmpClient) -> Result<Vec<u8>, String> {
    let mut memory = Vec::with_capacity((END - START) as usize);
    let mut at = START;
    while at < END {
        let n = ((END - at) as usize).min(4096);
        memory.extend(read(q, at, n)?);
        at += n as u32;
    }
    Ok(memory)
}
fn decode_blocks(memory: &[u8]) -> Result<Vec<HeapBlock>, String> {
    if memory.len() != (END - START) as usize {
        return Err("short heap snapshot".into());
    }
    let mut pos = 0usize;
    let mut blocks = Vec::new();
    while pos < memory.len() {
        let flags = u32_at(memory, pos + 4)?;
        let size = (flags & 0x7fff_ffff) as usize;
        if size < 8 || size % 8 != 0 || pos.checked_add(size).is_none_or(|end| end > memory.len()) {
            return Err("heap changed while sampling; retrying".into());
        }
        blocks.push(HeapBlock {
            address: START + pos as u32,
            size: size as u32,
            allocated: flags >> 31 != 0,
            owner: None,
        });
        pos += size;
    }
    Ok(blocks)
}
pub fn heap(q: &QmpClient, model: Model) -> Result<HeapSnapshot, String> {
    let before = guard(q, model)?;
    let memory = read_arena(q)?;
    let after = heap::read(q, model)?;
    if before != after {
        return Err("heap changed while sampling; retrying".into());
    }
    let mut blocks = decode_blocks(&memory)?;
    if blocks
        .iter()
        .filter(|b| !b.allocated)
        .map(|b| b.size)
        .sum::<u32>()
        != after.free
    {
        return Err("heap free blocks disagree with counters; retrying".into());
    }
    // Ownership is inferred only for stacks and TCBs that pass all task checks.
    if format!("{:x}", Sha256::digest(read(q, CREATE_TASK, 64)?)) == CREATE_HASH {
        let count = u32_at(&read(q, TASK_COUNT, 4)?, 0)?;
        let current = u32_at(&read(q, CURRENT_TCB, 4)?, 0)?;
        if count <= 256
            && let Ok(snapshot) = decode_tasks(&memory, count, current)
        {
            for task in snapshot.tasks {
                for block in &mut blocks {
                    if block.allocated && block.address + 8 == task.tcb {
                        block.owner = Some(format!("{} · task", task.name));
                    }
                    if block.allocated && block.address + 8 == task.stack_start {
                        block.owner = Some(format!("{} · stack", task.name));
                    }
                }
            }
        }
    }
    Ok(HeapSnapshot {
        counters: after,
        blocks,
    })
}
pub fn tasks(q: &QmpClient, model: Model) -> Result<TasksSnapshot, String> {
    let before = guard(q, model)?;
    if format!("{:x}", Sha256::digest(read(q, CREATE_TASK, 64)?)) != CREATE_HASH {
        return Err("Task layout is unsupported for this firmware".into());
    }
    let count = u32_at(&read(q, TASK_COUNT, 4)?, 0)?;
    if count > 256 {
        return Err("scheduler is not ready".into());
    }
    let memory = read_arena(q)?;
    let after = heap::read(q, model)?;
    let count_after = u32_at(&read(q, TASK_COUNT, 4)?, 0)?;
    let current_after = u32_at(&read(q, CURRENT_TCB, 4)?, 0)?;
    if before != after || count != count_after {
        return Err("tasks changed while sampling; retrying".into());
    }
    decode_tasks(&memory, count, current_after)
}
fn decode_tasks(memory: &[u8], expected: u32, current: u32) -> Result<TasksSnapshot, String> {
    if memory.len() != (END - START) as usize {
        return Err("short heap snapshot".into());
    }
    let mut pos = 0usize;
    let mut blocks = Vec::new();
    while pos < memory.len() {
        let flags = u32_at(memory, pos + 4)?;
        let size = (flags & 0x7fff_ffff) as usize;
        if size < 8 || size % 8 != 0 || pos.checked_add(size).is_none_or(|x| x > memory.len()) {
            return Err("heap changed while sampling; retrying".into());
        }
        blocks.push((pos, size, flags >> 31 != 0));
        pos += size;
    }
    let mut result = Vec::new();
    for &(pos, size, allocated) in &blocks {
        if !allocated || size != 0xa0 {
            continue;
        }
        let t = pos + 8;
        let tcb = START + t as u32;
        let stack = u32_at(memory, t + 0x30)?;
        let top = u32_at(memory, t + 0x64)?;
        if u32_at(memory, t + 0x10)? != tcb
            || u32_at(memory, t + 0x24)? != tcb
            || !(START..END).contains(&stack)
            || top < stack
            || top >= END
        {
            continue;
        }
        let name_bytes = &memory[t + 0x34..t + 0x64];
        let name_end = name_bytes
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(name_bytes.len());
        if !name_bytes[..name_end].iter().all(|c| (32..127).contains(c)) {
            continue;
        }
        let stack_offset = (stack - START) as usize;
        let Some(&(_, block_size, true)) = blocks.iter().find(|(p, _, _)| *p + 8 == stack_offset)
        else {
            continue;
        };
        if block_size < 16 {
            continue;
        }
        let stack_size = (block_size - 16) as u32;
        if stack.checked_add(stack_size).is_none_or(|end| end > END)
            || top != stack + stack_size - 8
        {
            continue;
        }
        let free = memory[stack_offset..stack_offset + stack_size as usize]
            .iter()
            .take_while(|b| **b == 0xa5)
            .count() as u32;
        let container = u32_at(memory, t + 0x14)?;
        let state = if current == tcb {
            "Current"
        } else if (0x808a3af4..0x808a3d74).contains(&container)
            && (container - 0x808a3af4) % 20 == 0
        {
            "Ready"
        } else if container == 0x8020aea8 || container == 0x8020aebc {
            "Delayed"
        } else if container == 0x8020ab80 {
            "Waiting"
        } else {
            "Other"
        };
        result.push(Task {
            state: state.into(),
            name: if name_end == 0 {
                "(unnamed)".into()
            } else {
                String::from_utf8_lossy(&name_bytes[..name_end]).into_owned()
            },
            tcb,
            priority: u32_at(memory, t + 0x2c)?,
            base_priority: u32_at(memory, t + 0x70)?,
            stack_start: stack,
            stack_size,
            stack_remaining: free,
            current: current == tcb,
        });
    }
    if result.len() != expected as usize {
        return Err(format!(
            "inconsistent task snapshot ({} of {expected}); retrying",
            result.len()
        ));
    }
    result.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.name.cmp(&b.name)));
    Ok(TasksSnapshot { tasks: result })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn put(mem: &mut [u8], offset: usize, word: u32) {
        mem[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
    }
    #[test]
    fn task_snapshot_finds_stack_and_rejects_mismatched_count() {
        let mut mem = vec![0u8; (END - START) as usize];
        put(&mut mem, 4, 0x8000_1010);
        mem[8..8 + 4096].fill(0xa5);
        mem[8 + 2048] = 0;
        let block = 0x1010usize;
        put(&mut mem, block + 4, 0x8000_00a0);
        let t = block + 8;
        let tcb = START + t as u32;
        let stack = START + 8;
        put(&mut mem, t + 0x10, tcb);
        put(&mut mem, t + 0x24, tcb);
        put(&mut mem, t + 0x30, stack);
        put(&mut mem, t + 0x64, stack + 4096 - 8);
        put(&mut mem, t + 0x2c, 14);
        put(&mut mem, t + 0x70, 12);
        mem[t + 0x34..t + 0x39].copy_from_slice(b"Audio");
        let free = block + 0xa0;
        let free_size = (mem.len() - free) as u32;
        put(&mut mem, free + 4, free_size);
        let blocks = decode_blocks(&mem).unwrap();
        assert_eq!(blocks.len(), 3);
        assert!(blocks[0].allocated && blocks[1].allocated);
        assert!(!blocks[2].allocated);
        let snapshot = decode_tasks(&mem, 1, tcb).unwrap();
        assert_eq!(snapshot.tasks[0].name, "Audio");
        assert_eq!(snapshot.tasks[0].stack_remaining, 2048);
        assert_eq!(snapshot.tasks[0].stack_size, 4096);
        assert_eq!(snapshot.tasks[0].state, "Current");
        assert_eq!(snapshot.tasks[0].base_priority, 12);
        assert!(decode_tasks(&mem, 2, tcb).is_err());
    }
}
