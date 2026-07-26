let
  valid = import ./valid.nix;
in
valid
// {
  members = valid.members // {
    qwen-a = valid.members.qwen-a // {
      classes = valid.members.qwen-a.classes ++ [ "not-declared" ];
    };
  };
}
