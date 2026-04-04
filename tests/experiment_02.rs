mod support;

type Scheduler = elude::experiment_02::Scheduler<()>;
type OverlapScheduler = elude::experiment_02::Scheduler<support::OverlapState>;

#[test]
fn empty_schedule_runs() {
    support::empty_schedule_runs::<Scheduler>();
}

#[test]
fn repeated_runs_execute_each_job_once() {
    support::repeated_runs_execute_each_job_once::<Scheduler>();
}

#[test]
fn exhaustive_pairwise_conflict_matrix() {
    support::exhaustive_pairwise_conflict_matrix::<Scheduler>();
}

#[test]
fn exhaustive_internal_dependency_matrix() {
    support::exhaustive_internal_dependency_matrix::<Scheduler>();
}

#[test]
fn duplicate_reads_within_a_job_are_allowed() {
    support::duplicate_reads_within_a_job_are_allowed::<Scheduler>();
}

#[test]
fn invalid_internal_conflicts_are_rejected() {
    support::invalid_internal_conflicts_are_rejected::<Scheduler>();
}

#[test]
fn unknown_dependency_is_a_strict_barrier() {
    support::unknown_dependency_is_a_strict_barrier::<Scheduler>();
}

#[test]
fn strict_write_chain_preserves_declaration_order() {
    support::strict_write_chain_preserves_declaration_order::<Scheduler>();
}

#[test]
fn strict_conflict_dominates_when_multiple_dependencies_are_present() {
    support::strict_conflict_dominates_when_multiple_dependencies_are_present::<Scheduler>();
}

#[test]
fn three_job_mixed_constraints_are_respected() {
    support::three_job_mixed_constraints_are_respected::<Scheduler>();
}

#[test]
fn independent_jobs_can_overlap() {
    support::independent_jobs_can_overlap::<OverlapScheduler>();
}

#[test]
fn shared_reads_can_overlap() {
    support::shared_reads_can_overlap::<OverlapScheduler>();
}
