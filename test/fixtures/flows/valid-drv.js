export const meta = {
  name: "fixture-valid-drv",
  description: "drv uses the implicit reserved build pool",
  pools: [],
  argsSchema: { type: "object" },
  maxNodes: 1
};

drv({
  drvPath: "/nix/store/00000000000000000000000000000000-fixture.drv",
  outputs: [
    {
      name: "out",
      path: "/nix/store/11111111111111111111111111111111-fixture"
    }
  ]
});
