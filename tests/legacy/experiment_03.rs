type Scheduler = elude::legacy::experiment_03::Scheduler<()>;
type OverlapScheduler = elude::legacy::experiment_03::Scheduler<super::support::OverlapState>;

#[test]
fn empty_schedule_runs() {
    super::support::empty_schedule_runs::<Scheduler>();
}

#[test]
fn repeated_runs_execute_each_job_once() {
    super::support::repeated_runs_execute_each_job_once::<Scheduler>();
}

#[test]
fn exhaustive_pairwise_conflict_matrix() {
    super::support::exhaustive_pairwise_conflict_matrix::<Scheduler>();
}

#[test]
fn exhaustive_internal_dependency_matrix() {
    super::support::exhaustive_internal_dependency_matrix::<Scheduler>();
}

#[test]
fn duplicate_reads_within_a_job_are_allowed() {
    super::support::duplicate_reads_within_a_job_are_allowed::<Scheduler>();
}

#[test]
fn invalid_internal_conflicts_are_rejected() {
    super::support::invalid_internal_conflicts_are_rejected::<Scheduler>();
}

#[test]
fn unknown_dependency_is_a_strict_barrier() {
    super::support::unknown_dependency_is_a_strict_barrier::<Scheduler>();
}

#[test]
fn strict_write_chain_preserves_declaration_order() {
    super::support::strict_write_chain_preserves_declaration_order::<Scheduler>();
}

#[test]
fn strict_conflict_dominates_when_multiple_dependencies_are_present() {
    super::support::strict_conflict_dominates_when_multiple_dependencies_are_present::<Scheduler>();
}

#[test]
fn three_job_mixed_constraints_are_respected() {
    super::support::three_job_mixed_constraints_are_respected::<Scheduler>();
}

#[test]
fn independent_jobs_can_overlap() {
    super::support::independent_jobs_can_overlap::<OverlapScheduler>();
}

#[test]
fn shared_reads_can_overlap() {
    super::support::shared_reads_can_overlap::<OverlapScheduler>();
}
