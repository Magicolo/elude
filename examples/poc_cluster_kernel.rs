use std::hint::black_box;
use std::time::Instant;

const JOBS: usize = 64;
const ROUNDS: usize = 500_000;

#[derive(Clone)]
struct KernelGraph {
    successor_lists: Vec<Vec<usize>>,
    predecessor_masks: [u64; JOBS],
    root_mask: u64,
    candidate_masks: [u64; JOBS],
    initial_waits: [u8; JOBS],
}

fn build_graph() -> KernelGraph {
    let mut successor_lists = vec![Vec::new(); JOBS];
    let mut predecessor_masks = [0u64; JOBS];

    for (job, predecessor_mask) in predecessor_masks.iter_mut().enumerate() {
        let layer = job / 8;
        let slot = job % 8;
        if layer == 0 {
            continue;
        }

        let mut predecessors = Vec::new();
        predecessors.push((layer - 1) * 8 + slot);
        if slot > 0 {
            predecessors.push((layer - 1) * 8 + (slot - 1));
        }
        if slot + 1 < 8 {
            predecessors.push((layer - 1) * 8 + (slot + 1));
        }
        if layer % 2 == 1 && layer > 1 {
            predecessors.push((layer - 2) * 8 + slot);
        }

        predecessors.sort_unstable();
        predecessors.dedup();

        for pred in predecessors {
            successor_lists[pred].push(job);
            *predecessor_mask |= 1u64 << pred;
        }
    }

    let mut candidate_masks = [0u64; JOBS];
    let mut initial_waits = [0u8; JOBS];
    let mut root_mask = 0u64;
    for job in 0..JOBS {
        candidate_masks[job] = successor_lists[job]
            .iter()
            .fold(0u64, |mask, &succ| mask | (1u64 << succ));
        initial_waits[job] = predecessor_masks[job].count_ones() as u8;
        if initial_waits[job] == 0 {
            root_mask |= 1u64 << job;
        }
    }

    KernelGraph {
        successor_lists,
        predecessor_masks,
        root_mask,
        candidate_masks,
        initial_waits,
    }
}

fn run_counter_kernel(graph: &KernelGraph) -> u64 {
    let mut checksum = 0u64;
    let mut waits = graph.initial_waits;
    let mut queue = [0usize; JOBS];
    let mut head = 0usize;
    let mut tail = 0usize;

    for (job, &wait) in waits.iter().enumerate() {
        if wait == 0 {
            queue[tail] = job;
            tail += 1;
        }
    }

    while head != tail {
        let job = queue[head];
        head += 1;
        checksum ^= (job as u64).wrapping_mul(0x9e37_79b9);
        for &successor in graph.successor_lists[job].iter() {
            waits[successor] -= 1;
            if waits[successor] == 0 {
                queue[tail] = successor;
                tail += 1;
            }
        }
    }

    checksum
}

fn run_mask_kernel(graph: &KernelGraph) -> u64 {
    let mut checksum = 0u64;
    let mut done_mask = 0u64;
    let mut ready_mask = graph.root_mask;
    let mut candidate_mask = 0u64;

    while ready_mask != 0 {
        let bit = ready_mask.trailing_zeros() as usize;
        let job_mask = 1u64 << bit;
        ready_mask &= ready_mask - 1;
        done_mask |= job_mask;
        checksum ^= (bit as u64).wrapping_mul(0x9e37_79b9);
        candidate_mask |= graph.candidate_masks[bit];

        let mut candidates = candidate_mask;
        while candidates != 0 {
            let candidate = candidates.trailing_zeros() as usize;
            let candidate_bit = 1u64 << candidate;
            candidates &= candidates - 1;
            if graph.predecessor_masks[candidate] & !done_mask == 0 {
                ready_mask |= candidate_bit;
                candidate_mask &= !candidate_bit;
            }
        }
    }

    checksum
}

fn main() {
    let graph = build_graph();
    println!("cluster kernel poc: jobs={JOBS} rounds={ROUNDS}");

    let start = Instant::now();
    let mut counter_checksum = 0u64;
    for _ in 0..ROUNDS {
        counter_checksum ^= run_counter_kernel(&graph);
    }
    let counter_elapsed = start.elapsed();

    let start = Instant::now();
    let mut mask_checksum = 0u64;
    for _ in 0..ROUNDS {
        mask_checksum ^= run_mask_kernel(&graph);
    }
    let mask_elapsed = start.elapsed();

    black_box(counter_checksum ^ mask_checksum);

    println!(
        "counter_kernel total={counter_elapsed:?} avg_round={:.2}ns",
        counter_elapsed.as_secs_f64() * 1_000_000_000.0 / ROUNDS as f64
    );
    println!(
        "mask_kernel    total={mask_elapsed:?} avg_round={:.2}ns",
        mask_elapsed.as_secs_f64() * 1_000_000_000.0 / ROUNDS as f64
    );
    println!(
        "speedup mask_vs_counter={:.2}x",
        counter_elapsed.as_secs_f64() / mask_elapsed.as_secs_f64()
    );
}
