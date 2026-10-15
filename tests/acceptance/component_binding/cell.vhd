-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this file,
-- You can obtain one at http://mozilla.org/MPL/2.0/.

library ieee;
use ieee.std_logic_1164.all;

entity cell is
  port (
    q : out std_logic);
end entity;

architecture a of cell is
begin
  q <= '1';
end architecture;
