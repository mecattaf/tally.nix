{
  classes.review.diversity = [ "maker" ];
  pools = [ "worker-gpu" ];
  members.local = {
    family = "fixture";
    maker = null;
    classes = [ "review" ];
    adapter = "pi";
    pools = [ "worker-gpu" ];
    launch.model = "fixture";
  };
}
