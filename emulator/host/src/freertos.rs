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
const DELAY_TASK: u32 = 0x80084e90;
const DELAY_HASH: &str = "eb976fe0ec661701842cb3f7bfde2fe23b1f173d16cfbe6b08c730dfd6f071ce";
const QUEUE_CREATE: u32 = 0x800909c0;
const QUEUE_HASH: &str = "53e5aeb59bdbec2225c9b6d5a692d578a7c992bd68aba55034397e27250ef0dc";
const TASK_CHANGE: u32 = 0x8020ab38;
const TICK_TASK: u32 = 0x80091a20;
const TICK_HASH: &str = "b16ed9b761ad29feb71b01d07d53be5c1da56e467692d2e31424f4653227cd64";

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
    pub creation_number: u32,
    pub mutexes_held: u32,
    pub event_list: Option<u32>,
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
    pub yield_pending: bool,
    pub tick_overflows: u32,
    pub next_wake: Option<u32>,
    pub critical_nesting: u32,
}
pub fn scheduler(q: &QmpClient, model: Model) -> Result<SchedulerSnapshot, String> {
    guard(q, model)?;
    if format!("{:x}", Sha256::digest(read(q, CREATE_TASK, 64)?)) != CREATE_HASH
        || format!("{:x}", Sha256::digest(read(q, TICK_TASK, 64)?)) != TICK_HASH
    {
        return Err("Scheduler layout is unsupported for this firmware".into());
    }
    let b = read(q, TASK_COUNT, 0x2c)?;
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
        yield_pending: u32_at(&b, 0x18)? != 0,
        tick_overflows: u32_at(&b, 0x28)?,
        next_wake: match u32_at(&b, 0x20)? {
            u32::MAX => None,
            value => Some(value),
        },
        critical_nesting: u32_at(&read(q, 0x80207634, 4)?, 0)?,
    })
}
#[derive(Clone, Debug)]
pub struct TasksSnapshot {
    pub tasks: Vec<Task>,
}
#[derive(Clone, Debug)]
pub struct Object {
    pub name: &'static str,
    pub address: u32,
    pub count: u32,
    pub capacity: u32,
    pub send_waiters: u32,
    pub receive_waiters: u32,
}
pub fn objects(q: &QmpClient, model: Model) -> Result<Vec<Object>, String> {
    let before = guard(q, model)?;
    if format!("{:x}", Sha256::digest(read(q, QUEUE_CREATE, 64)?)) != QUEUE_HASH {
        return Err("Object layout is unsupported for this firmware".into());
    }
    let root = 0x8020a2a0;
    let address = u32_at(&read(q, root, 4)?, 0)?;
    if address == 0 {
        return Ok(Vec::new());
    }
    if address < START + 8
        || address.checked_add(0x50).is_none_or(|end| end > END)
        || address % 8 != 0
    {
        return Err("Notification semaphore is not ready".into());
    }
    let header = read(q, address - 8, 8)?;
    let flags = u32_at(&header, 4)?;
    let size = flags & 0x7fff_ffff;
    if flags >> 31 == 0
        || size < 0x58
        || address
            .checked_sub(8)
            .and_then(|a| a.checked_add(size))
            .is_none_or(|end| end > END)
    {
        return Err("Notification semaphore allocation is inconsistent; retrying".into());
    }
    let b = read(q, address, 0x50)?;
    let again = read(q, address, 0x50)?;
    if b != again || u32_at(&read(q, root, 4)?, 0)? != address || before != heap::read(q, model)? {
        return Err("Object changed while sampling; retrying".into());
    }
    Ok(vec![decode_notification(address, &b)?])
}
fn decode_notification(address: u32, b: &[u8]) -> Result<Object, String> {
    let count = u32_at(b, 0x38)?;
    let capacity = u32_at(b, 0x3c)?;
    let send_waiters = u32_at(b, 0x10)?;
    let receive_waiters = u32_at(b, 0x24)?;
    if capacity != 1
        || count > capacity
        || u32_at(b, 0x40)? != 0
        || b.get(0x4c) != Some(&2)
        || send_waiters > 256
        || receive_waiters > 256
    {
        return Err("Notification semaphore layout is inconsistent; retrying".into());
    }
    Ok(Object {
        name: "Notification semaphore",
        address,
        count,
        capacity,
        send_waiters,
        receive_waiters,
    })
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
    if format!("{:x}", Sha256::digest(read(q, DELAY_TASK, 64)?)) != DELAY_HASH {
        return Err("Task wait layout is unsupported for this firmware".into());
    }
    let change = u32_at(&read(q, TASK_CHANGE, 4)?, 0)?;
    let count = u32_at(&read(q, TASK_COUNT, 4)?, 0)?;
    if count > 256 {
        return Err("scheduler is not ready".into());
    }
    let memory = read_arena(q)?;
    let after = heap::read(q, model)?;
    let count_after = u32_at(&read(q, TASK_COUNT, 4)?, 0)?;
    let current_after = u32_at(&read(q, CURRENT_TCB, 4)?, 0)?;
    if before != after || count != count_after || change != u32_at(&read(q, TASK_CHANGE, 4)?, 0)? {
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
        let event = u32_at(memory, t + 0x28)?;
        let word_at = |address: u32| {
            address
                .checked_sub(START)
                .and_then(|offset| u32_at(memory, offset as usize).ok())
        };
        let item = tcb + 0x18;
        let event_valid = event != 0
            && blocks.iter().any(|(p, size, allocated)| {
                *allocated
                    && event >= START + *p as u32 + 8
                    && event
                        .checked_add(20)
                        .is_some_and(|end| end <= START + (*p + *size) as u32)
            })
            && word_at(event).is_some_and(|count| (1..=256).contains(&count))
            && u32_at(memory, t + 0x1c)
                .ok()
                .and_then(|next| next.checked_add(8))
                .and_then(word_at)
                == Some(item)
            && u32_at(memory, t + 0x20)
                .ok()
                .and_then(|prev| prev.checked_add(4))
                .and_then(word_at)
                == Some(item);
        let state = if current == tcb {
            "Current"
        } else if (0x808a3af4..0x808a3d74).contains(&container)
            && (container - 0x808a3af4) % 20 == 0
        {
            "Ready"
        } else if container == 0x8020aea8 || container == 0x8020aebc {
            if event_valid {
                "Timed wait"
            } else {
                "Delay / wait"
            }
        } else if container == 0x8020ab80 {
            if event_valid {
                "Blocked"
            } else {
                "Wait / susp."
            }
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
            creation_number: u32_at(memory, t + 0x68)?,
            mutexes_held: u32_at(memory, t + 0x74)?,
            event_list: event_valid.then_some(event),
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
    fn notification_requires_the_known_semaphore_shape() {
        let mut mem = [0u8; 0x50];
        put(&mut mem, 0x38, 1);
        put(&mut mem, 0x3c, 1);
        mem[0x4c] = 2;
        assert_eq!(decode_notification(START + 8, &mem).unwrap().count, 1);
        put(&mut mem, 0x38, 2);
        assert!(decode_notification(START + 8, &mem).is_err());
        put(&mut mem, 0x38, 1);
        mem[0x4c] = 0;
        assert!(decode_notification(START + 8, &mem).is_err());
        assert!(decode_notification(START + 8, &mem[..16]).is_err());
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

        // A task on an indefinite wait list can be identified as blocked only
        // when its event item and the allocated list link back to each other.
        put(&mut mem, free + 4, 0x8000_0080);
        let rest = free + 0x80;
        let rest_size = (mem.len() - rest) as u32;
        put(&mut mem, rest + 4, rest_size);
        let list = START + free as u32 + 8;
        let sentinel = list + 8;
        let item = tcb + 0x18;
        put(&mut mem, free + 8, 1);
        put(&mut mem, free + 8 + 12, item);
        put(&mut mem, free + 8 + 16, item);
        put(&mut mem, t + 0x14, 0x8020ab80);
        put(&mut mem, t + 0x28, list);
        put(&mut mem, t + 0x1c, sentinel);
        put(&mut mem, t + 0x20, sentinel);
        assert_eq!(decode_tasks(&mem, 1, 0).unwrap().tasks[0].state, "Blocked");
        put(&mut mem, t + 0x14, 0x8020aea8);
        assert_eq!(
            decode_tasks(&mem, 1, 0).unwrap().tasks[0].state,
            "Timed wait"
        );
        put(&mut mem, free + 8 + 16, 0);
        assert_eq!(
            decode_tasks(&mem, 1, 0).unwrap().tasks[0].state,
            "Delay / wait"
        );
    }
}
