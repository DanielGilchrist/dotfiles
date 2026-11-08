use agent_core::activity::{Lineage, Pid};
use sysinfo::{Pid as SysPid, ProcessRefreshKind, ProcessesToUpdate, System};

const MAX_DEPTH: usize = 64;

/// `start` and its ancestors, nearest first. Untraced when the chain is cut
/// at the start: the first parent can't be read, or is already init, as when
/// the agent died while its hook ran. The caller can then tell "unknown"
/// from a chain that doesn't hold the process it looks for.
pub fn of(start: u32, mut parent: impl FnMut(u32) -> Option<u32>) -> Lineage {
    let Some(first) = parent(start).filter(|p| *p > 1 && *p != start) else {
        return Lineage::Untraced;
    };
    let mut chain = vec![start, first];
    while chain.len() < MAX_DEPTH {
        let current = chain[chain.len() - 1];
        match parent(current).filter(|p| *p > 1 && !chain.contains(p)) {
            Some(ancestor) => chain.push(ancestor),
            None => break,
        }
    }
    Lineage::Traced(chain.into_iter().filter_map(Pid::new).collect())
}

/// A parent lookup over one `System`, refreshing only the process asked
/// about.
pub fn process_table() -> impl FnMut(u32) -> Option<u32> {
    let mut system = System::new();
    move |pid| {
        let pid = SysPid::from_u32(pid);
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        system.process(pid)?.parent().map(SysPid::as_u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tree(pairs: &[(u32, u32)]) -> impl FnMut(u32) -> Option<u32> {
        let map: HashMap<u32, u32> = pairs.iter().copied().collect();
        move |pid| map.get(&pid).copied()
    }

    fn traced(pids: &[u32]) -> Lineage {
        Lineage::Traced(pids.iter().filter_map(|p| Pid::new(*p)).collect())
    }

    #[test]
    fn walks_up_to_init() {
        let parents = tree(&[(30, 20), (20, 10), (10, 1)]);
        assert_eq!(of(30, parents), traced(&[30, 20, 10]));
    }

    #[test]
    fn stops_at_an_unreadable_parent_after_the_first() {
        let parents = tree(&[(30, 20)]);
        assert_eq!(of(30, parents), traced(&[30, 20]));
    }

    #[test]
    fn is_untraced_when_the_first_parent_is_unreadable() {
        assert_eq!(of(30, tree(&[])), Lineage::Untraced);
    }

    #[test]
    fn an_orphan_of_init_is_untraced_not_a_stranger() {
        assert_eq!(of(30, tree(&[(30, 1)])), Lineage::Untraced);
    }

    #[test]
    fn survives_a_cycle() {
        let parents = tree(&[(3, 2), (2, 3)]);
        assert_eq!(of(3, parents), traced(&[3, 2]));
    }

    #[test]
    fn reads_this_process_ancestry() {
        let Lineage::Traced(chain) = of(std::process::id(), process_table()) else {
            panic!("this process has a readable parent");
        };
        assert!(chain.len() > 1);
    }
}
