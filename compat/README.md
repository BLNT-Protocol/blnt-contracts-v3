# SDK 27 compatibility crates

These crates preserve the Blend v2 dependency APIs while compiling the
candidate contracts with Soroban SDK 27:

- `sep-40-oracle` is adapted from Script3 `sep-40-oracle` at
  `4778f5c2b3e86321f07ee043e71b5b1f774e5817` and retains its MIT license.
- `sep-41-token` is adapted from Script3 `sep-41-token` at
  `cd95d680474d150efb328d2ac1e03b9c651b2e58` and retains its MIT license.
- `soroban-fixed-point-math` is adapted from Script3
  `soroban-fixed-point-math` at
  `062517b22fa659fb408d649392a9d2d4799055fa` and retains its MIT license.
- `moderc3156` is a local ABI declaration for the callback used by Blend v2,
  licensed with the rest of this repository under AGPL-3.0.
