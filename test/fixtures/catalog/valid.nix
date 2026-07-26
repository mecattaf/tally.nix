{
  classes = {
    pooled-fast.diversity = [
      "family"
      "maker"
    ];
    pooled-strongest.diversity = [
      "family"
      "maker"
    ];
    coding = { };
  };

  pools = [ "worker-gpu" ];

  members = {
    qwen-a = {
      order = 10;
      family = "qwen";
      maker = "alibaba";
      classes = [
        "pooled-fast"
        "pooled-strongest"
        "coding"
      ];
      adapter = "pi";
      pools = [ "worker-gpu" ];
      launch.model = "qwen-a";
    };
    qwen-b = {
      order = 20;
      family = "qwen";
      maker = "community";
      classes = [
        "pooled-fast"
        "pooled-strongest"
        "coding"
      ];
      adapter = "pi";
      pools = [ "worker-gpu" ];
      launch.model = "qwen-b";
    };
    llama-a = {
      order = 30;
      family = "llama";
      maker = "meta";
      classes = [
        "pooled-fast"
        "pooled-strongest"
        "coding"
      ];
      adapter = "pi";
      pools = [ "worker-gpu" ];
      launch.model = "llama-a";
    };
    mistral-a = {
      order = 40;
      family = "mistral";
      maker = "mistral";
      classes = [
        "pooled-fast"
        "pooled-strongest"
        "coding"
      ];
      adapter = "pi";
      pools = [ "worker-gpu" ];
      launch.model = "mistral-a";
    };
    llama-b = {
      order = 50;
      family = "llama";
      maker = "community";
      classes = [
        "pooled-fast"
        "pooled-strongest"
        "coding"
      ];
      adapter = "pi";
      pools = [ "worker-gpu" ];
      launch.model = "llama-b";
    };
  };
}
