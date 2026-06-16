#[derive(Clone, Copy, Debug)]
enum Policy {
    CriticalPath,
    ReadHeavy,
    UnlockBalance,
}

impl Policy {
    fn name(self) -> &'static str {
        match self {
            Self::CriticalPath => "critical_path",
            Self::ReadHeavy => "read_heavy",
            Self::UnlockBalance => "unlock_balance",
        }
    }

    fn score_bridge(self, ready_conflicting_reads: i32, blocked_successors: i32) -> i32 {
        match self {
            Self::CriticalPath => i32::MAX,
            Self::ReadHeavy => i32::MIN,
            Self::UnlockBalance => blocked_successors - ready_conflicting_reads,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Scenario {
    name: &'static str,
    ready_conflicting_reads: i32,
    blocked_successors: i32,
}

fn main() {
    let scenarios = [
        Scenario {
            name: "pre_heavy",
            ready_conflicting_reads: 96,
            blocked_successors: 32,
        },
        Scenario {
            name: "post_heavy",
            ready_conflicting_reads: 32,
            blocked_successors: 96,
        },
    ];

    println!("bridge policy poc: structural frontier choice");
    println!("bridge score > 0 means: run bridge now instead of preserving the current read batch");

    for scenario in scenarios {
        println!(
            "\nscenario={} ready_conflicting_reads={} blocked_successors={}",
            scenario.name, scenario.ready_conflicting_reads, scenario.blocked_successors
        );

        for policy in [
            Policy::CriticalPath,
            Policy::ReadHeavy,
            Policy::UnlockBalance,
        ] {
            let score = policy.score_bridge(
                scenario.ready_conflicting_reads,
                scenario.blocked_successors,
            );
            let decision = if score > 0 {
                "bridge_first"
            } else {
                "preserve_reads"
            };
            println!(
                "{:16} score={:12} decision={}",
                policy.name(),
                score,
                decision
            );
        }
    }
}
