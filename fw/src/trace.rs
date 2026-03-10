use core::sync::atomic::AtomicU32;

use embassy_sync::blocking_mutex::{Mutex, raw::ThreadModeRawMutex};
use embassy_time::Instant;
use heapless::Vec;

const MAX_TASKS: usize = 16;

const ENABLE_TRACE: bool = false;

#[derive(Clone)]
pub struct SystemProfile {
    pub idle_ticks: u64,
    pub busy_ticks: u64,
}

impl SystemProfile {
    const fn new() -> Self {
        Self {
            idle_ticks: 0,
            busy_ticks: 0,
        }
    }
}

#[derive(Clone)]
pub struct TaskProfile {
    pub name: &'static str,
    pub run_ticks: u64,
    pub longest_execution: u64,
}

pub struct Profile {
    pub system: SystemProfile,
    pub tasks: Vec<TaskProfile, MAX_TASKS>,
}

struct TaskState {
    name: &'static str,
    task_id: u32,
    start_time: Instant,
    run_time: u64,
    longest_exec: u64,
}

impl TaskState {
    const fn new() -> Self {
        Self {
            name: "unknown",
            task_id: 0xdeadbeef,
            start_time: Instant::from_millis(0),
            run_time: 0,
            longest_exec: 0,
        }
    }
}

struct TraceState {
    tasks: Vec<TaskState, MAX_TASKS>,
    active_task: usize,

    executor_transition_time: Instant,
    profile: SystemProfile,
}

impl TraceState {
    const fn new() -> Self {
        Self {
            tasks: Vec::new(),
            active_task: 0,
            executor_transition_time: Instant::from_millis(0),
            profile: SystemProfile::new(),
        }
    }

    fn with_task(&mut self, task_id: u32) -> Option<&mut TaskState> {
        if let Some(idx) = self.tasks.iter().position(|t| t.task_id == task_id) {
            return Some(&mut self.tasks[idx]);
        } else {
            let mut task = TaskState::new();
            //task.name = name;
            task.task_id = task_id;
            if self.tasks.push(task).is_ok() {
                let idx = self.tasks.len() - 1;
                Some(&mut self.tasks[idx])
            } else {
                None
            }
        }
    }
}

static WAKE_TIME: [AtomicU32; MAX_TASKS] = [const { AtomicU32::new(0) }; MAX_TASKS];
static TASK_IDS: [AtomicU32; MAX_TASKS] = [const { AtomicU32::new(0xdeadbeef) }; MAX_TASKS];

static TRACE: Mutex<ThreadModeRawMutex, TraceState> = Mutex::new(TraceState::new());

pub fn set_task_name(task_id: u32, name: &'static str) {
    if ENABLE_TRACE {
        // Safety: Not re-enterant
        unsafe {
            TRACE.lock_mut(|trace| {
                if let Some(task) = trace.with_task(task_id) {
                    task.name = name;
                }
            });
        }
    }
}

pub fn reset_profile() {
    if ENABLE_TRACE {
        // Safety: Not re-enterant
        unsafe {
            TRACE.lock_mut(|trace| {
                trace.profile = SystemProfile::new();
                for task in trace.tasks.iter_mut() {
                    task.run_time = 0;
                    task.longest_exec = 0;
                }
            });
        }
    }
}

pub fn take_profile() -> Profile {
    if ENABLE_TRACE {
        TRACE.lock(|trace| Profile {
            system: trace.profile.clone(),
            tasks: trace
                .tasks
                .iter()
                .map(|t| TaskProfile {
                    name: t.name,
                    run_ticks: t.run_time,
                    longest_execution: t.longest_exec,
                })
                .collect(),
        })
    } else {
        Profile {
            system: SystemProfile::new(),
            tasks: Vec::new(),
        }
    }
}

/// This callback is called when the executor begins polling. This will always
/// be paired with a later call to `_embassy_trace_executor_idle`.
///
/// This marks the EXECUTOR state transition from IDLE -> SCHEDULING.
#[unsafe(no_mangle)]
fn _embassy_trace_poll_start(executor_id: u32) {
    if ENABLE_TRACE {
        // Safety: Not re-enterant
        unsafe {
            TRACE.lock_mut(|trace| {
                let now = Instant::now();
                trace.profile.idle_ticks = trace
                    .profile
                    .idle_ticks
                    .saturating_add((now - trace.executor_transition_time).as_ticks());
                trace.executor_transition_time = now;
            });
        }
    }
}

/// This callback is called AFTER a task is initialized/allocated, and BEFORE
/// it is enqueued to run for the first time. If the task ends (and does not
/// loop "forever"), there will be a matching call to `_embassy_trace_task_end`.
///
/// Tasks start life in the SPAWNED state.
#[unsafe(no_mangle)]
fn _embassy_trace_task_new(executor_id: u32, task_id: u32) {}

/// This callback is called AFTER a task is destructed/freed. This will always
/// have a prior matching call to `_embassy_trace_task_new`.
#[unsafe(no_mangle)]
fn _embassy_trace_task_end(executor_id: u32, task_id: u32) {}

/// This callback is called AFTER a task has been dequeued from the runqueue,
/// and BEFORE the task is polled. There will always be a matching call to
/// `_embassy_trace_task_exec_end`.
///
/// This marks the TASK state transition from WAITING -> RUNNING
/// This marks the EXECUTOR state transition from SCHEDULING -> POLLING
#[unsafe(no_mangle)]
fn _embassy_trace_task_exec_begin(executor_id: u32, task_id: u32) {
    if ENABLE_TRACE {
        // Safety: Not re-enterant
        unsafe {
            TRACE.lock_mut(|trace| {
                if let Some(task) = trace.with_task(task_id) {
                    task.start_time = Instant::now();
                }
            });
        }
    }
}

/// This callback is called AFTER a task has completed polling. There will
/// always be a matching call to `_embassy_trace_task_exec_begin`.
///
/// This marks the TASK state transition from either:
/// * RUNNING -> IDLE - if there were no `_embassy_trace_task_ready_begin` events
///     for this task since the last `_embassy_trace_task_exec_begin` for THIS task
/// * RUNNING -> WAITING - if there WAS a `_embassy_trace_task_ready_begin` event
///     for this task since the last `_embassy_trace_task_exec_begin` for THIS task
///
/// This marks the EXECUTOR state transition from POLLING -> SCHEDULING
#[unsafe(no_mangle)]
fn _embassy_trace_task_exec_end(excutor_id: u32, task_id: u32) {
    if ENABLE_TRACE {
        // Safety: Not re-enterant
        unsafe {
            TRACE.lock_mut(|trace| {
                if let Some(task) = trace.with_task(task_id) {
                    let now = Instant::now();
                    let exec_time = (now - task.start_time).as_ticks();
                    task.run_time += exec_time;
                    if exec_time > task.longest_exec {
                        task.longest_exec = exec_time;
                    }
                }
            });
        }
    }
}

/// This callback is called AFTER the waker for a task is awoken, and BEFORE it
/// is added to the run queue.
///
/// If the given task is currently RUNNING, this marks no state change, BUT the
/// RUNNING task will then move to the WAITING stage when polling is complete.
///
/// If the given task is currently IDLE, this marks the TASK state transition
/// from IDLE -> WAITING.
///
/// NOTE: This may be called from an interrupt, outside the context of the current
/// task or executor.
#[unsafe(no_mangle)]
fn _embassy_trace_task_ready_begin(executor_id: u32, task_id: u32) {
    if ENABLE_TRACE {
        // Find the right task
        for i in 0..MAX_TASKS {
            if TASK_IDS[i].load(core::sync::atomic::Ordering::Relaxed) == task_id {
                // Set the wake time
                let now = Instant::now();
                WAKE_TIME[i].store(
                    (now.as_millis() & 0xffff_ffff) as u32,
                    core::sync::atomic::Ordering::Relaxed,
                );
                break;
            }
        }
    }
}

/// This callback is called AFTER all dequeued tasks in a single call to poll
/// have been processed. This will always be paired with a call to
/// `_embassy_trace_executor_idle`.
///
/// This marks the EXECUTOR state transition from SCHEDULING -> IDLE
#[unsafe(no_mangle)]
fn _embassy_trace_executor_idle(executor_id: u32) {
    if ENABLE_TRACE {
        // Safety: Not re-enterant
        unsafe {
            TRACE.lock_mut(|trace| {
                let now = Instant::now();
                trace.profile.busy_ticks = trace
                    .profile
                    .busy_ticks
                    .saturating_add((now - trace.executor_transition_time).as_ticks());
                trace.executor_transition_time = now;
            });
        }
    }
}
