# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this file,
# You can obtain one at http://mozilla.org/MPL/2.0/.
#
# Copyright (c) 2014-2026, Lars Asplund lars.anders.asplund@gmail.com
# Copyright (c) 2026, xq-Tec <info@xq-tec.com>
#
# AI NOTICE: Generated, minimally reviewed.


"""
Test the risim-ghdl interface
"""

import unittest
from pathlib import Path
import os
from shutil import rmtree
from unittest import mock
from tests.unit.test_test_bench import Entity
from vunit.sim_if.risim_ghdl import RisimGHDLInterface
from vunit.project import Project
from vunit.ostools import renew_path, write_file
from vunit.exceptions import CompileError
from vunit.configuration import Configuration
from vunit.vhdl_standard import VHDL


class TestRisimGHDLInterface(unittest.TestCase):
    """
    Test the risim-ghdl interface
    """

    @mock.patch("vunit.sim_if.check_output", autospec=True, return_value="")  # pylint: disable=no-self-use
    @mock.patch.object(RisimGHDLInterface, "determine_version", return_value=5.0)
    def test_compile_project_2008(self, determine_version, check_output):
        simif = RisimGHDLInterface(prefix="prefix", output_path="")
        write_file("file.vhd", "")

        project = Project()
        project.add_library("lib", "lib_path")
        project.add_source_file("file.vhd", "lib", file_type="vhdl", vhdl_standard=VHDL.standard("2008"))
        simif.compile_project(project)
        check_output.assert_called_once_with(
            [
                str(Path("prefix") / "risim-ghdl"),
                "-a",
                "--workdir=lib_path",
                "--work=lib",
                "--std=08",
                "-Plib_path",
                "file.vhd",
            ],
            env=simif.get_env(),
        )

    @mock.patch("vunit.sim_if.check_output", autospec=True, return_value="")  # pylint: disable=no-self-use
    @mock.patch.object(RisimGHDLInterface, "determine_version", return_value=5.0)
    def test_compile_project_extra_flags(self, determine_version, check_output):
        simif = RisimGHDLInterface(prefix="prefix", output_path="")
        write_file("file.vhd", "")

        project = Project()
        project.add_library("lib", "lib_path")
        source_file = project.add_source_file("file.vhd", "lib", file_type="vhdl")
        source_file.set_compile_option("risim-ghdl.a_flags", ["custom", "flags"])
        simif.compile_project(project)
        check_output.assert_called_once_with(
            [
                str(Path("prefix") / "risim-ghdl"),
                "-a",
                "--workdir=lib_path",
                "--work=lib",
                "--std=08",
                "-Plib_path",
                "custom",
                "flags",
                "file.vhd",
            ],
            env=simif.get_env(),
        )

    @mock.patch.object(RisimGHDLInterface, "determine_version", return_value=5.0)
    def test_elaborate_e_project(self, determine_version):
        design_unit = Entity("tb_entity", file_name=str(Path("tempdir") / "file.vhd"))
        design_unit.original_file_name = str(Path("tempdir") / "other_path" / "original_file.vhd")
        design_unit.generic_names = ["runner_cfg", "tb_path"]

        config = Configuration("name", design_unit, sim_options={"risim-ghdl.elab_e": True})

        simif = RisimGHDLInterface(prefix="prefix", output_path="")
        simif._vhdl_standard = VHDL.standard("2008")  # pylint: disable=protected-access
        simif._project = Project()  # pylint: disable=protected-access
        simif._project.add_library("lib", "lib_path")  # pylint: disable=protected-access

        self.assertEqual(
            simif._get_command(  # pylint: disable=protected-access
                config, str(Path("output_path") / "risim-ghdl"), True, True
            ),
            [
                str(Path("prefix") / "risim-ghdl"),
                "-e",
                "--std=08",
                "--work=lib",
                "--workdir=lib_path",
                "-Plib_path",
                "tb_entity",
                "arch",
            ],
        )

    @mock.patch.object(RisimGHDLInterface, "determine_version", return_value=5.0)
    def test_simulate_command(self, determine_version):
        design_unit = Entity("tb_entity", file_name=str(Path("tempdir") / "file.vhd"))
        design_unit.generic_names = ["runner_cfg"]

        config = Configuration("name", design_unit, sim_options={"risim-ghdl.elab_flags": ["--flag"]})
        config.generics["runner_cfg"] = "seed"

        simif = RisimGHDLInterface(prefix="prefix", output_path="")
        simif._vhdl_standard = VHDL.standard("2008")  # pylint: disable=protected-access
        simif._project = Project()  # pylint: disable=protected-access
        simif._project.add_library("lib", "lib_path")  # pylint: disable=protected-access

        command = simif.simulate_command(
            output_path=str(Path("output_path") / "suite"),
            test_suite_name="lib.tb_entity",
            config=config,
        )

        self.assertEqual(
            command,
            [
                str(Path("prefix") / "risim-ghdl"),
                "--elab-run",
                "--std=08",
                "--work=lib",
                "--workdir=lib_path",
                "-Plib_path",
                "--flag",
                "tb_entity",
                "arch",
                "-grunner_cfg=seed",
                "--assert-level=error",
            ],
        )

    @mock.patch.object(RisimGHDLInterface, "determine_version", return_value=5.0)
    def test_compile_project_verilog_error(self, determine_version):
        simif = RisimGHDLInterface(prefix="prefix", output_path="")
        write_file("file.v", "")

        project = Project()
        project.add_library("lib", "lib_path")
        project.add_source_file("file.v", "lib", file_type="verilog")
        self.assertRaises(CompileError, simif.compile_project, project)

    @mock.patch.dict(os.environ, {"VUNIT_RISIM_GHDL_PATH": "/custom/risim-ghdl/bin"})
    def test_find_prefix_from_env(self):
        self.assertEqual(RisimGHDLInterface.find_prefix(), "/custom/risim-ghdl/bin")

    @mock.patch.object(
        RisimGHDLInterface,
        "_get_version_output",
        return_value="GHDL 6.0.0-risim (tarball) [simulation adapter]\n",
    )
    def test_determine_version(self, _get_version_output):
        self.assertEqual(RisimGHDLInterface.determine_version("prefix"), 6.0)

    def setUp(self):
        self.output_path = str(Path(__file__).parent / "test_risim_ghdl_interface_out")
        renew_path(self.output_path)
        self.project = Project()
        self.cwd = os.getcwd()
        os.chdir(self.output_path)

    def tearDown(self):
        os.chdir(self.cwd)
        if Path(self.output_path).exists():
            rmtree(self.output_path)
