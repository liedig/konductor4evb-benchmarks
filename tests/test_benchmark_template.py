#!/usr/bin/env python3
"""Structural checks for benchmark-stack.yaml.

Run: `uv run --with pytest --with pyyaml pytest tests` from this repo.

Parses the template with a CloudFormation-tag-tolerant YAML loader and
asserts the harness surface from docs/plan/design.md decision 5: the
function's single-concurrency lock, its Delete-policy log group, the env
wiring the handler resolves, and a least-privilege role (execute-api Invoke
scoped to the core API stage, bindings-table Put/DeleteItem, EventBridge
rule management scoped to the benchmark domain bus, PutEvents and bus
management scoped to the throwaway floor bus, CloudWatch/Logs/SQS reads).
"""
import unittest
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parent.parent
TEMPLATE = REPO / "benchmark-stack.yaml"


class CfnLoader(yaml.SafeLoader):
    """SafeLoader that maps !Ref/!Sub/!GetAtt/... tags to plain mappings."""


def _cfn_tag(loader, suffix, node):
    if isinstance(node, yaml.ScalarNode):
        value = loader.construct_scalar(node)
        if suffix == "GetAtt" and "." in value:
            return {"Fn::GetAtt": value.split(".", 1)}
        return {suffix: value}
    if isinstance(node, yaml.SequenceNode):
        return {suffix: loader.construct_sequence(node, deep=True)}
    return {suffix: loader.construct_mapping(node, deep=True)}


CfnLoader.add_multi_constructor("!", _cfn_tag)


def load_template():
    return yaml.load(TEMPLATE.read_text(), Loader=CfnLoader)


def rendered(value):
    """Flattens !Sub strings and refs so tests can substring-match ARNs."""
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        if "Sub" in value:
            return value["Sub"]
        if "Ref" in value:
            return f"${{{value['Ref']}}}"
        if "Fn::GetAtt" in value:
            return ".".join(value["Fn::GetAtt"])
        return " ".join(rendered(v) for v in value.values())
    if isinstance(value, list):
        return " ".join(rendered(v) for v in value)
    return str(value)


class BenchmarkTemplateTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.doc = load_template()
        cls.parameters = cls.doc["Parameters"]
        cls.resources = cls.doc["Resources"]
        cls.function = cls.resources["BenchmarkFunction"]["Properties"]

    def statements(self):
        policies = self.function["Policies"]
        out = []
        for policy in policies:
            out.extend(policy["Statement"])
        return out

    def actions_for(self, prefix):
        """All actions across statements whose sid starts with prefix."""
        for stmt in self.statements():
            if stmt["Sid"].startswith(prefix):
                return stmt
        self.fail(f"no statement with Sid {prefix}*")

    # ---- function shape ----

    def test_single_concurrency_locks_runs(self):
        self.assertEqual(self.function["ReservedConcurrentExecutions"], 1)

    def test_timeout_covers_the_default_profile(self):
        # 30s warmup + 420s measured + floor probe + 120s settle + collect
        self.assertGreaterEqual(self.function["Timeout"], 900)

    def test_rust_cargolambda_build(self):
        meta = self.resources["BenchmarkFunction"].get("Metadata", {})
        self.assertEqual(meta.get("BuildMethod"), "rust-cargolambda")
        self.assertEqual(self.function["Runtime"], "provided.al2023")
        self.assertEqual(self.function["Handler"], "bootstrap")
        self.assertIn("arm64", self.function["Architectures"])

    def test_log_group_deleted_on_teardown(self):
        group = self.resources["BenchmarkLogGroup"]
        self.assertEqual(group.get("DeletionPolicy"), "Delete")
        self.assertEqual(group.get("UpdateReplacePolicy"), "Delete")
        self.assertIn("konductor-publish-benchmark", rendered(group["Properties"]["LogGroupName"]))

    # ---- env wiring the handler resolves ----

    def test_env_wiring(self):
        env = self.function["Environment"]["Variables"]
        for key in (
            "BENCH_DOMAIN",
            "BENCH_API_ENDPOINT",
            "BENCH_BINDINGS_TABLE",
            "BENCH_DOMAINS_TABLE",
            "BENCH_ACCOUNT_ID",
            "BENCH_ORG",
            "BENCH_ENV",
        ):
            self.assertIn(key, env, f"env var {key} missing")

    # ---- parameters ----

    def test_domain_parameter_has_no_default(self):
        param = self.parameters["BenchDomain"]
        self.assertNotIn("Default", param, "the operator picks the domain deliberately")

    def test_resolution_parameters_exist(self):
        for name in ("Organization", "Environment", "CoreApiEndpoint", "CoreApiId", "CoreApiStage"):
            self.assertIn(name, self.parameters, f"parameter {name} missing")

    # ---- least-privilege role ----

    def test_execute_api_invoke_scoped_to_the_core_api_stage(self):
        stmt = self.actions_for("InvokePlatformApi")
        self.assertEqual(stmt["Action"], "execute-api:Invoke")
        resource = rendered(stmt["Resource"])
        self.assertIn("execute-api", resource)
        self.assertIn("${CoreApiId}", resource)
        self.assertIn("${CoreApiStage}", resource)
        self.assertNotEqual(resource.strip(), "*")

    def test_domain_status_read_scoped_to_the_domains_table(self):
        # The ACTIVE check reads the domains table directly: the platform's
        # describe endpoint requires an owner binding, and the harness only
        # seeds a publisher binding.
        self.assertIn("DomainsTableName", self.parameters, "DomainsTableName parameter missing")
        self.assertEqual(self.parameters["DomainsTableName"].get("Default"), "konductor-domains")
        stmt = self.actions_for("DomainStatusRead")
        self.assertEqual(stmt["Action"], ["dynamodb:GetItem"])
        resource = rendered(stmt["Resource"])
        self.assertIn("dynamodb", resource)
        self.assertIn("${DomainsTableName}", resource)

    def test_binding_fixture_is_put_and_delete_only(self):
        stmt = self.actions_for("BindingFixture")
        self.assertEqual(sorted(stmt["Action"]), ["dynamodb:DeleteItem", "dynamodb:PutItem"])
        self.assertIn("${BindingsTableName}", rendered(stmt["Resource"]))

    def test_arrival_rule_management_scoped_to_the_domain_bus(self):
        stmt = self.actions_for("ArrivalRule")
        for action in ("events:PutRule", "events:PutTargets", "events:RemoveTargets", "events:DeleteRule"):
            self.assertIn(action, stmt["Action"])
        resource = rendered(stmt["Resource"])
        self.assertIn("rule/", resource)
        self.assertIn("${BenchDomain}", resource, "the rule ARN pins the benchmark domain bus")
        self.assertIn("konductor-benchmark-", resource, "only harness rules are manageable")

    def test_floor_put_events_scoped_to_the_throwaway_bus(self):
        stmt = self.actions_for("FloorProbe")
        self.assertEqual(stmt["Action"], "events:PutEvents")
        self.assertIn("konductor-benchmark-floor", rendered(stmt["Resource"]))

    def test_floor_bus_lifecycle_scoped_to_the_throwaway_bus(self):
        stmt = self.actions_for("FloorBus")
        self.assertEqual(sorted(stmt["Action"]), ["events:CreateEventBus", "events:DeleteEventBus"])
        self.assertIn("konductor-benchmark-floor", rendered(stmt["Resource"]))

    def test_metric_read_is_get_metric_statistics(self):
        stmt = self.actions_for("StageMetrics")
        self.assertEqual(stmt["Action"], "cloudwatch:GetMetricStatistics")

    def test_arrival_log_group_management_is_scoped(self):
        stmt = self.actions_for("ArrivalLogGroup")
        for action in ("logs:CreateLogGroup", "logs:DeleteLogGroup"):
            self.assertIn(action, stmt["Action"])
        self.assertIn("konductor-benchmark-arrival", rendered(stmt["Resource"]))

    def test_arrival_resource_policy_is_account_level(self):
        # PutResourcePolicy/DeleteResourcePolicy do not support
        # resource-level permissions; scoping them to the log group ARN
        # makes CloudWatch Logs deny the call.
        stmt = self.actions_for("ArrivalResourcePolicy")
        self.assertEqual(
            sorted(stmt["Action"]),
            ["logs:DeleteResourcePolicy", "logs:PutResourcePolicy"],
        )
        self.assertEqual(stmt["Resource"], "*")
    def test_insights_queries_read_the_arrival_log_group(self):
        stmt = self.actions_for("ArrivalQuery")
        for action in ("logs:StartQuery", "logs:GetQueryResults", "logs:StopQuery"):
            self.assertIn(action, stmt["Action"])
        self.assertIn("konductor-benchmark-arrival", rendered(stmt["Resource"]))

    def test_publish_dlq_read(self):
        stmt = self.actions_for("PublishDlq")
        self.assertEqual(sorted(stmt["Action"]), ["sqs:GetQueueAttributes", "sqs:GetQueueUrl"])
        self.assertIn("${PublishDlqName}", rendered(stmt["Resource"]))

    def test_function_logging_is_scoped_to_its_own_group(self):
        stmt = self.actions_for("FunctionLogging")
        for action in ("logs:CreateLogStream", "logs:PutLogEvents"):
            self.assertIn(action, stmt["Action"])
        self.assertIn("konductor-publish-benchmark", rendered(stmt["Resource"]))


if __name__ == "__main__":
    unittest.main()
