{
  classes.dormant = { };
  pools = [ "worker-gpu" ];
  members.filtered = {
    enable = false;
    classes = [ "dormant" ];
  };
}
