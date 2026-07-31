{ pkgs }:

pkgs.writeShellApplication {
  name = "spec-build-driver";
  runtimeInputs = [
    pkgs.gh
    pkgs.git
    pkgs.python3
  ];
  text = ''
    exec ${pkgs.python3}/bin/python3 ${../../examples/flows/spec_build_driver.py} "$@"
  '';
}
