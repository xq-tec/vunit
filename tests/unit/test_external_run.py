# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this file,
# You can obtain one at http://mozilla.org/MPL/2.0/.
#
# Copyright (c) 2026, xq-Tec <info@xq-tec.com>

# AI NOTICE: Generated, not reviewed.

"""
Tests for externally executed simulation runs
"""

import unittest
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path
from unittest import mock

from tests.unit.test_test_bench import Entity
from vunit.configuration import Configuration
from vunit.ostools import renew_path, write_file
from vunit.test.report import PASSED, FAILED
from vunit.test.suites import TestRun, get_result_file_name


class RecordingSimulator:
    """
    Simulator interface that records simulate_command calls
    """

    name = "mock"
    output_path = "/simulator/out"
    use_color = False

    def __init__(self):
        self.commands = []

    def has_valid_exit_code(self):
        return False

    def simulate_command(self, output_path, test_suite_name, config):  # pylint: disable=unused-argument
        command = ["mock-sim", output_path, test_suite_name]
        self.commands.append(command)
        return command

    def simulate(self, output_path, test_suite_name, config, elaborate_only):  # pylint: disable=unused-argument
        del output_path, test_suite_name, config, elaborate_only
        return True


class TestExternalRun(unittest.TestCase):
    """
    Test prepare/finish on TestRun
    """

    def setUp(self):
        self.output_path = str(Path(__file__).parent / "test_external_run_out")
        renew_path(self.output_path)

    def tearDown(self):
        renew_path(self.output_path)

    def test_prepare_and_finish(self):
        design_unit = Entity("tb_entity", file_name=str(Path("tempdir") / "file.vhd"))
        design_unit.generic_names = ["runner_cfg"]
        config = Configuration("default", design_unit)
        simulator = RecordingSimulator()

        test_run = TestRun(
            simulator_if=simulator,
            config=config,
            elaborate_only=False,
            test_suite_name="lib.tb_entity.all",
            test_cases=["all"],
            seed="seed",
        )

        command = test_run.prepare(self.output_path)
        self.assertEqual(command, ["mock-sim", self.output_path, "lib.tb_entity.all"])

        write_file(
            get_result_file_name(self.output_path),
            "test_start:all\ntest_suite_done\n",
        )

        results = test_run.finish(self.output_path, True, lambda: "")
        self.assertEqual(results, {"all": PASSED})

    def test_finish_marks_failed_on_bad_exit_code(self):
        design_unit = Entity("tb_entity", file_name=str(Path("tempdir") / "file.vhd"))
        design_unit.generic_names = ["runner_cfg"]
        config = Configuration("default", design_unit)
        simulator = RecordingSimulator()
        simulator.has_valid_exit_code = mock.Mock(return_value=True)

        test_run = TestRun(
            simulator_if=simulator,
            config=config,
            elaborate_only=False,
            test_suite_name="lib.tb_entity.all",
            test_cases=["all"],
            seed="seed",
        )
        test_run.prepare(self.output_path)
        write_file(
            get_result_file_name(self.output_path),
            "test_start:all\ntest_suite_done\n",
        )

        results = test_run.finish(self.output_path, False, lambda: "")
        self.assertEqual(results, {"all": FAILED})


def run_output_path(entry):
    """
    Returns the per-suite output directory for a collected run command entry.
    """
    return str(Path(entry["output_file_name"]).parent)


class MockRunCommands:
    """
    Collect run commands like the PyO3 RunCommands type
    """

    def __init__(self):
        self.entries = []

    def append(self, test_suite_name, output_file_name):
        self.entries.append(
            {
                "test_suite_name": test_suite_name,
                "output_file_name": output_file_name,
            }
        )


class TestPrepareRunCommands(unittest.TestCase):
    """
    Test VUnit.prepare_run_commands and incremental finish
    """

    def setUp(self):
        from vunit.ui import VUnit

        self.VUnit = VUnit
        self.output_path = str(Path(__file__).parent / "test_prepare_run_commands_out")
        renew_path(self.output_path)

    def tearDown(self):
        renew_path(self.output_path)

    def _make_ui(self, create_tests):
        args = mock.Mock(
            test_patterns=["*"],
            with_attributes=None,
            without_attributes=None,
            seed=None,
            elaborate=False,
        )
        ui = object.__new__(self.VUnit)
        ui._args = args
        ui._output_path = self.output_path
        ui._printer = mock.Mock()
        ui._test_filter = lambda **kwargs: True
        ui._external_run_state = None
        ui._simulator_if = RecordingSimulator()
        ui._update_test_history = mock.Mock()
        ui._create_tests = create_tests
        return ui

    @staticmethod
    def _single_suite_test_run():
        design_unit = Entity("tb_entity", file_name=str(Path("tempdir") / "file.vhd"))
        design_unit.generic_names = ["runner_cfg"]
        config = Configuration("default", design_unit)
        return TestRun(
            simulator_if=RecordingSimulator(),
            config=config,
            elaborate_only=False,
            test_suite_name="lib.tb_entity.all",
            test_cases=["all"],
            seed="seed",
        )

    @staticmethod
    def _suite(name, test_run):
        class Suite:
            pass

        suite = Suite()
        suite.name = name
        suite.test_names = [name]
        suite._run = test_run
        suite.get_seed = staticmethod(lambda: "seed")
        return suite

    def _prepare(self, ui, test_list):
        from vunit.test.list import TestList

        collect_commands = MockRunCommands()
        with redirect_stdout(StringIO()):
            with mock.patch.object(ui, "_create_external_run_mapping_file"):
                ui.prepare_run_commands(["*"], collect_commands)
        return collect_commands, test_list

    @mock.patch("vunit.ui.VUnit._create_external_run_mapping_file")
    def test_prepare_and_finalize_run_commands(self, _mapping):
        from vunit.test.list import TestList

        test_run = self._single_suite_test_run()
        test_list = TestList()
        test_list.add_suite(self._suite("lib.tb_entity.all", test_run))
        ui = self._make_ui(mock.Mock(return_value=test_list))

        collect_commands, _ = self._prepare(ui, test_list)
        self.assertEqual(len(collect_commands.entries), 1)
        entry = collect_commands.entries[0]
        self.assertEqual(entry["test_suite_name"], "lib.tb_entity.all")
        command = ui.start_run_command("lib.tb_entity.all")
        self.assertEqual(command[0], "mock-sim")

        write_file(
            get_result_file_name(run_output_path(entry)),
            "test_start:all\ntest_suite_done\n",
        )

        stdout = StringIO()
        with redirect_stdout(stdout):
            self.assertTrue(ui.finish_run_command("lib.tb_entity.all", True))
            all_ok = ui.finalize_run_commands()
        self.assertTrue(all_ok)
        self.assertEqual(stdout.getvalue(), "")
        ui._update_test_history.assert_called_once()

    @mock.patch("vunit.ui.VUnit._create_external_run_mapping_file")
    def test_multi_suite_finish_order(self, _mapping):
        from vunit.test.list import TestList

        test_run_a = self._single_suite_test_run()
        test_run_b = self._single_suite_test_run()
        test_run_b._test_suite_name = "lib.tb_other.all"
        test_run_b._test_cases = ["all"]

        test_list = TestList()
        test_list.add_suite(self._suite("lib.tb_entity.all", test_run_a))
        test_list.add_suite(self._suite("lib.tb_other.all", test_run_b))
        ui = self._make_ui(mock.Mock(return_value=test_list))

        collect_commands, _ = self._prepare(ui, test_list)
        self.assertEqual(len(collect_commands.entries), 2)

        for entry in collect_commands.entries:
            ui.start_run_command(entry["test_suite_name"])
            write_file(
                get_result_file_name(run_output_path(entry)),
                "test_start:all\ntest_suite_done\n",
            )

        ui.finish_run_command("lib.tb_entity.all", True)
        ui.finish_run_command("lib.tb_other.all", True)
        self.assertTrue(ui.finalize_run_commands())

    @mock.patch("vunit.ui.VUnit._create_external_run_mapping_file")
    def test_failure_isolation(self, _mapping):
        from vunit.test.list import TestList

        test_run_a = self._single_suite_test_run()
        test_run_a._simulator_if.has_valid_exit_code = mock.Mock(return_value=True)
        test_run_b = self._single_suite_test_run()
        test_run_b._test_suite_name = "lib.tb_other.all"

        test_list = TestList()
        test_list.add_suite(self._suite("lib.tb_entity.all", test_run_a))
        test_list.add_suite(self._suite("lib.tb_other.all", test_run_b))
        ui = self._make_ui(mock.Mock(return_value=test_list))

        collect_commands, _ = self._prepare(ui, test_list)

        for entry in collect_commands.entries:
            ui.start_run_command(entry["test_suite_name"])
            write_file(
                get_result_file_name(run_output_path(entry)),
                "test_start:all\ntest_suite_done\n",
            )

        self.assertFalse(ui.finish_run_command("lib.tb_entity.all", False))
        self.assertTrue(ui.finish_run_command("lib.tb_other.all", True))
        self.assertFalse(ui.finalize_run_commands())

        report = ui._update_test_history.call_args[0][0]
        self.assertTrue(report.has_test("lib.tb_entity.all"))
        self.assertTrue(report.has_test("lib.tb_other.all"))
        self.assertFalse(report.result_of("lib.tb_entity.all").passed)
        self.assertTrue(report.result_of("lib.tb_other.all").passed)

    @mock.patch("vunit.ui.VUnit._create_external_run_mapping_file")
    def test_pre_config_failure_at_start(self, _mapping):
        from vunit.test.list import TestList

        test_run = self._single_suite_test_run()
        test_list = TestList()
        test_list.add_suite(self._suite("lib.tb_entity.all", test_run))
        ui = self._make_ui(mock.Mock(return_value=test_list))

        collect_commands, _ = self._prepare(ui, test_list)
        self.assertEqual(len(collect_commands.entries), 1)

        with mock.patch.object(test_run, "prepare", return_value=None):
            self.assertIsNone(ui.start_run_command("lib.tb_entity.all"))

        entry = ui._external_run_state["run_suites_by_name"]["lib.tb_entity.all"]
        self.assertTrue(entry["finished"])
        self.assertFalse(ui.finalize_run_commands())

    @mock.patch("vunit.ui.VUnit._create_external_run_mapping_file")
    def test_double_finish_raises(self, _mapping):
        from vunit.test.list import TestList

        test_run = self._single_suite_test_run()
        test_list = TestList()
        test_list.add_suite(self._suite("lib.tb_entity.all", test_run))
        ui = self._make_ui(mock.Mock(return_value=test_list))

        collect_commands, _ = self._prepare(ui, test_list)
        ui.start_run_command("lib.tb_entity.all")
        write_file(
            get_result_file_name(run_output_path(collect_commands.entries[0])),
            "test_start:all\ntest_suite_done\n",
        )

        ui.finish_run_command("lib.tb_entity.all", True)
        with self.assertRaises(RuntimeError):
            ui.finish_run_command("lib.tb_entity.all", True)

    @mock.patch("vunit.ui.VUnit._create_external_run_mapping_file")
    def test_start_twice_raises(self, _mapping):
        from vunit.test.list import TestList

        test_run = self._single_suite_test_run()
        test_list = TestList()
        test_list.add_suite(self._suite("lib.tb_entity.all", test_run))
        ui = self._make_ui(mock.Mock(return_value=test_list))

        self._prepare(ui, test_list)
        ui.start_run_command("lib.tb_entity.all")
        with self.assertRaises(RuntimeError):
            ui.start_run_command("lib.tb_entity.all")

    @mock.patch("vunit.ui.VUnit._create_external_run_mapping_file")
    def test_abort_clears_state(self, _mapping):
        from vunit.test.list import TestList

        test_run = self._single_suite_test_run()
        test_list = TestList()
        test_list.add_suite(self._suite("lib.tb_entity.all", test_run))
        ui = self._make_ui(mock.Mock(return_value=test_list))

        self._prepare(ui, test_list)
        ui.abort_run_commands()
        self.assertIsNone(ui._external_run_state)

        collect_commands, _ = self._prepare(ui, test_list)
        self.assertEqual(len(collect_commands.entries), 1)

    @mock.patch("vunit.ui.VUnit._create_external_run_mapping_file")
    def test_finalize_marks_unfinished_failed(self, _mapping):
        from vunit.test.list import TestList

        test_run_a = self._single_suite_test_run()
        test_run_b = self._single_suite_test_run()
        test_run_b._test_suite_name = "lib.tb_other.all"

        test_list = TestList()
        test_list.add_suite(self._suite("lib.tb_entity.all", test_run_a))
        test_list.add_suite(self._suite("lib.tb_other.all", test_run_b))
        ui = self._make_ui(mock.Mock(return_value=test_list))

        collect_commands, _ = self._prepare(ui, test_list)
        ui.start_run_command("lib.tb_entity.all")
        write_file(
            get_result_file_name(run_output_path(collect_commands.entries[0])),
            "test_start:all\ntest_suite_done\n",
        )

        ui.finish_run_command("lib.tb_entity.all", True)
        self.assertFalse(ui.finalize_run_commands())
        report = ui._update_test_history.call_args[0][0]
        self.assertFalse(report.result_of("lib.tb_other.all").passed)
