# run-configuration Specification

## Purpose
TBD - created by archiving change validate-abort-min-samples. Update Purpose after archive.

## Requirements

### Requirement: Abort guard override validation

The benchmark SHALL validate the `abort_min_samples` payload override at invocation time. A value of `0` MUST be rejected with an error naming the parameter, because the load phase's early-abort guard relies on a positive minimum to keep a single early failure from killing the run.

#### Scenario: Zero override rejected

- **WHEN** a run is invoked with a payload containing `"abort_min_samples": 0`
- **THEN** the invocation fails during configuration resolution
- **AND** the error message identifies `abort_min_samples` as the invalid parameter

#### Scenario: Positive override accepted

- **WHEN** a run is invoked with a payload containing a positive `abort_min_samples` value
- **THEN** configuration resolution succeeds with that value in effect

#### Scenario: Omitted override uses the default

- **WHEN** a run is invoked without an `abort_min_samples` payload key
- **THEN** configuration resolution succeeds with the default of 100
