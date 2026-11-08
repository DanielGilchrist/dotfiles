use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const MAX_DEPTH: usize = 64;

/// `start` and its ancestors, nearest first. None when even the first
/// parent can't be read, so the caller can tell "unknown" from "no match".
pub fn of(start: u32, parent: impl Fn(u32) -> Option<u32>) -> Option<Vec<u32>> {
    let mut chain = vec![start];
    let mut current = start;
    while chain.len() < MAX_DEPTH {
        let Some(next) = parent(current).filter(|p| *p > 1 && !chain.contains(p)) else {
            break;
        };
        chain.push(next);
        current = next;
    }
    (chain.len() > 1).then_some(chain)
}

pub fn parent_of(pid: u32) -> Option<u32> {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system.process(pid)?.parent().map(Pid::as_u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tree(pairs: &[(u32, u32)]) -> impl Fn(u32) -> Option<u32> {
        let map: HashMap<u32, u32> = pairs.iter().copied().collect();
        move |pid| map.get(&pid).copied()
    }

    #[test]
    fn walks_up_to_init() {
        let parents = tree(&[(30, 20), (20, 10), (10, 1)]);
        assert_eq!(of(30, parents), Some(vec![30, 20, 10]));
    }

    #[test]
    fn stops_at_an_unreadable_parent() {
        let parents = tree(&[(30, 20)]);
        assert_eq!(of(30, parents), Some(vec![30, 20]));
    }

    #[test]
    fn is_unknown_when_nothing_is_readable() {
        assert_eq!(of(30, tree(&[])), None);
    }

    #[test]
    fn survives_a_cycle() {
        let parents = tree(&[(3, 2), (2, 3)]);
        assert_eq!(of(3, parents), Some(vec![3, 2]));
    }

    #[test]
    fn reads_this_process_ancestry() {
        let chain = of(std::process::id(), parent_of).expect("readable");
        assert!(chain.len() > 1);
    }
}
