let
  valid = import ./valid.nix;
in
valid
// {
  members = valid.members // {
    qwen-a = valid.members.qwen-a // {
      pools = [ "not-declared" ];
    };
  };
}
