{ pkgs }:

let
  drivers = import ./campaign-drivers.nix { inherit pkgs; };
in
pkgs.writeShellApplication {
  name = "spec-build-driver";
  runtimeInputs = [
    pkgs.gh
    pkgs.git
    pkgs.python3
  ];
  text = ''
    exec ${pkgs.python3}/bin/python3 ${drivers}/spec_build_driver.py "$@"
  '';
}
