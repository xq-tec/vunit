# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this file,
# You can obtain one at http://mozilla.org/MPL/2.0/.
#
# Copyright (c) 2014-2026, Lars Asplund lars.anders.asplund@gmail.com

"""
Interface for risim-ghdl simulator
"""

import os
from pathlib import Path
from os import environ, makedirs
import logging
import subprocess
import re
from json import dump
from ..exceptions import CompileError
from ..ostools import Process
from . import SimulatorInterface, ListOfStringOption, BooleanOption
from . import check_executable
from ..vhdl_standard import VHDL

LOGGER = logging.getLogger(__name__)


class RisimGHDLInterface(SimulatorInterface):  # pylint: disable=too-many-instance-attributes
    """
    Interface for risim-ghdl simulator
    """

    name = "risim-ghdl"
    executable = environ.get("RISIM_GHDL", "risim-ghdl")

    compile_options = [
        ListOfStringOption("risim-ghdl.a_flags"),
        ListOfStringOption("risim-ghdl.flags"),  # Removed in v5.0.0
    ]

    sim_options = [
        ListOfStringOption("risim-ghdl.sim_flags"),
        ListOfStringOption("risim-ghdl.elab_flags"),
        BooleanOption("risim-ghdl.elab_e"),
    ]

    @staticmethod
    def add_arguments(parser):
        """
        Add command line arguments
        """
        parser.add_argument(
            "--risim-ghdl",
            metavar="PATH",
            default=None,
            help=(
                "Use risim-ghdl as simulator with the executable at PATH. "
                "Ignores VUNIT_RISIM_GHDL_PATH and PATH lookup."
            ),
        )

    @classmethod
    def from_args(cls, args, output_path, **kwargs):
        """
        Create instance from args namespace
        """
        explicit_path = getattr(args, "risim_ghdl", None)
        if explicit_path is not None:
            executable_path = Path(explicit_path).resolve()
            prefix = str(executable_path.parent)
            executable = executable_path.name
            return cls(
                output_path=output_path,
                prefix=prefix,
                gui=args.gui,
                executable=executable,
            )

        prefix = cls.find_prefix()
        check_executable("RISIM_GHDL", prefix, cls.executable)

        return cls(
            output_path=output_path,
            prefix=prefix,
            gui=args.gui,
        )

    @classmethod
    def find_prefix(cls):
        """
        Find prefix by looking at VUNIT_RISIM_GHDL_PATH environment variable
        """
        prefix = os.environ.get("VUNIT_RISIM_GHDL_PATH", None)
        if prefix is not None:
            return prefix
        return cls.find_prefix_from_path()

    @classmethod
    def find_prefix_from_path(cls):
        """
        Find first valid risim-ghdl toolchain prefix
        """
        return cls.find_toolchain([cls.executable])

    def __init__(self, output_path, prefix, *, gui=False, executable=None):
        SimulatorInterface.__init__(self, output_path, gui)

        self._prefix = prefix
        if executable is not None:
            self.executable = executable
        self._project = None
        self._vhdl_standard = None
        self._version = self.determine_version(self._prefix, self.executable)

    def has_valid_exit_code(self):  # pylint: disable=arguments-differ
        """
        Return if the simulation should fail with nonzero exit codes
        """
        return self._vhdl_standard >= VHDL.STD_2008

    @classmethod
    def _get_version_output(cls, prefix, executable=None):
        """
        Get the output of 'risim-ghdl --version'
        """
        if executable is None:
            executable = cls.executable
        return subprocess.check_output([str(Path(prefix) / executable), "--version"]).decode()

    @classmethod
    def determine_version(cls, prefix, executable=None):
        """
        Determine the risim-ghdl version
        """
        if executable is None:
            executable = cls.executable
        match = re.match(
            r"GHDL ([0-9]+\.[0-9]+).*?\[simulation adapter\]",
            cls._get_version_output(prefix, executable),
        )
        if match is None:
            output = cls._get_version_output(prefix, executable)
            raise ValueError(
                "Could not determine risim-ghdl version from 'risim-ghdl --version' output:\n" + output
            )
        return float(match.group(1))

    @classmethod
    def supports_vhdl_call_paths(cls):
        """
        Returns True when this simulator supports VHDL-2019 call paths
        """
        return False

    @classmethod
    def supports_vhdl_package_generics(cls):
        """
        Returns True when this simulator supports VHDL package generics
        """
        return True

    @classmethod
    def supports_vhpi(cls):
        """
        Returns True when the simulator supports VHPI
        """
        return cls.determine_version(cls.find_prefix()) > 0.36

    def setup_library_mapping(self, project):
        """
        Setup library mapping
        """
        self._project = project
        for library in project.get_libraries():
            if not Path(library.directory).exists():
                makedirs(library.directory)

        vhdl_standards = set(
            source_file.get_vhdl_standard()
            for source_file in project.get_source_files_in_order()
            if source_file.is_vhdl
        )

        if not vhdl_standards:
            self._vhdl_standard = VHDL.STD_2008
        elif len(vhdl_standards) != 1:
            raise RuntimeError(f"risim-ghdl cannot handle mixed VHDL standards, found {list(vhdl_standards)!r}")
        else:
            self._vhdl_standard = list(vhdl_standards)[0]

    def compile_source_file_command(self, source_file):
        """
        Returns the command to compile a single source_file
        """
        if source_file.is_vhdl:
            return self.compile_vhdl_file_command(source_file)

        LOGGER.error("Unknown file type: %s", source_file.file_type)
        raise CompileError

    def _std_str(self, vhdl_standard):
        """
        Convert standard to format of risim-ghdl command line flag
        """
        if vhdl_standard == VHDL.STD_2019:
            if self._version >= 6.0:
                return "19"
            raise ValueError("VHDL-2019 requires risim-ghdl >=6.0.0.")

        if vhdl_standard == VHDL.STD_2008:
            return "08"

        if vhdl_standard == VHDL.STD_2002:
            return "02"

        if vhdl_standard == VHDL.STD_1993:
            return "93"

        raise ValueError(f"Invalid VHDL standard {vhdl_standard!s}")

    def compile_vhdl_file_command(self, source_file):
        """
        Returns the command to compile a vhdl file
        """
        if source_file.compile_options.get("risim-ghdl.flags", []) != []:
            raise RuntimeError("'risim-ghdl.flags was removed in v5.0.0; use 'risim-ghdl.a_flags' instead")

        cmd = [
            str(Path(self._prefix) / self.executable),
            "-a",
            f"--workdir={source_file.library.directory!s}",
            f"--work={source_file.library.name!s}",
            f"--std={self._std_str(source_file.get_vhdl_standard())!s}",
        ]
        for library in self._project.get_libraries():
            cmd += [f"-P{library.directory!s}"]

        cmd += source_file.compile_options.get("risim-ghdl.a_flags", [])
        cmd += [source_file.name]
        return cmd

    def _get_command(
        self, config, output_path, elaborate_only, elab_e
    ):  # pylint: disable=too-many-branches,too-many-arguments,too-many-positional-arguments
        """
        Return risim-ghdl simulation command
        """
        cmd = [str(Path(self._prefix) / self.executable)]

        cmd += ["-e"] if elab_e else ["--elab-run"]

        cmd += [f"--std={self._std_str(self._vhdl_standard)!s}"]
        cmd += [f"--work={config.library_name!s}"]
        cmd += [f"--workdir={self._project.get_library(config.library_name).directory!s}"]
        cmd += [f"-P{lib.directory!s}" for lib in self._project.get_libraries()]

        cmd += config.sim_options.get("risim-ghdl.elab_flags", [])

        if config.vhdl_configuration_name is not None:
            cmd += [config.vhdl_configuration_name]
        else:
            cmd += [config.entity_name, config.architecture_name]

        sim = config.sim_options.get("risim-ghdl.sim_flags", [])
        for name, value in config.generics.items():
            sim += [f"-g{name!s}={value!s}"]
        sim += [f"--assert-level={config.vhdl_assert_stop_level!s}"]
        if config.sim_options.get("disable_ieee_warnings", False):
            sim += ["--ieee-asserts=disable"]

        if not elab_e:
            cmd += sim
            if elaborate_only:
                cmd += ["--no-run"]
        else:
            try:
                makedirs(output_path, mode=0o777)
            except OSError:
                pass
            with (Path(output_path) / "args.json").open("w", encoding="utf-8") as fname:
                dump(
                    {
                        "bin": str(Path(output_path) / f"{config.entity_name!s}-{config.architecture_name!s}"),
                        "build": cmd[1:],
                        "sim": sim,
                    },
                    fname,
                )

        return cmd

    def simulate_command(self, output_path, test_suite_name, config):  # pylint: disable=unused-argument
        """
        Return the command to simulate with entity as top level using generics.
        """
        script_path = str(Path(output_path) / self.name)

        if not Path(script_path).exists():
            makedirs(script_path)

        return self._get_command(config, script_path, elaborate_only=False, elab_e=False)

    def simulate(self, output_path, test_suite_name, config, elaborate_only):  # pylint: disable=too-many-locals,unused-argument
        """
        Simulate with entity as top level using generics

        The `elaborate_only` argument is ignored, because risim-ghdl doesn't support elaborate-only mode.
        """
        cmd = self.simulate_command(output_path, test_suite_name, config)

        status = True

        try:
            proc = Process(cmd)
            proc.consume_output()
        except Process.NonZeroExitCode:
            status = False

        return status
