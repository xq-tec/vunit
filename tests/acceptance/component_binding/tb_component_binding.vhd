-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this file,
-- You can obtain one at http://mozilla.org/MPL/2.0/.

-- Instantiates the component `cell` of library `cells` from library `top`, once made visible by a
-- use clause and once by an expanded name. Only the default binding binds the instances to the
-- entity `cells.cell`, so the testbench passes only if that entity is compiled. An unbound
-- instance leaves its output at 'U'.

library ieee;
use ieee.std_logic_1164.all;

library vunit_lib;
context vunit_lib.vunit_context;

library cells;
use cells.cells_pkg.all;

entity tb_component_binding is
  generic (
    runner_cfg : string);
end entity;

architecture a of tb_component_binding is
  signal q_used : std_logic;
  signal q_expanded : std_logic;
begin
  used : cell
    port map (
      q => q_used);

  expanded : cells.cells_pkg.cell
    port map (
      q => q_expanded);

  main : process
  begin
    test_runner_setup(runner, runner_cfg);
    wait for 1 ns;
    check_equal(q_used, '1', "instance through the use clause");
    check_equal(q_expanded, '1', "instance through the expanded name");
    test_runner_cleanup(runner);
  end process;
end architecture;
