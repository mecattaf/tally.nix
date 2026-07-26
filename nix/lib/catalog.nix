{
  lib,
  self,
}:

let
  inherit (lib)
    concatLists
    filterAttrs
    flatten
    mapAttrsToList
    mkOption
    optionalAttrs
    types
    unique
    ;

  nonEmptyString =
    value: builtins.isString value && builtins.match ".*[^[:space:]].*" value != null;

  nullableStringOption =
    description:
    mkOption {
      type = types.nullOr types.str;
      default = null;
      inherit description;
    };

  launchType = types.submodule {
    options = {
      prePromptArgv = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "Direct argv inserted before the member prompt.";
      };
      environment = mkOption {
        type = types.attrsOf types.str;
        default = { };
        description = "Environment supplied by the selected adapter launch.";
      };
      approvalPolicy = nullableStringOption "Named adapter approval policy.";
      sandboxPolicy = nullableStringOption "Named adapter sandbox policy.";
      model = nullableStringOption "Model value passed through the selected adapter.";
      effort = nullableStringOption "Optional reasoning-effort value passed through the selected adapter.";
    };
  };

  memberType = types.submodule (
    { name, ... }:
    {
      options = {
        enable = mkOption {
          type = types.bool;
          default = true;
          description = "Whether this roster member is emitted into the catalog.";
        };
        order = mkOption {
          type = types.ints.unsigned;
          default = 1000;
          description = "Catalog order; ties are resolved by the member attribute name.";
        };
        id = mkOption {
          type = types.str;
          default = name;
          description = "Stable member identity; defaults to the member attribute name.";
        };
        family = nullableStringOption "Model family used by family diversity.";
        maker = nullableStringOption "Model maker used by maker diversity.";
        classes = mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "Declared selector classes containing this member.";
        };
        adapter = nullableStringOption "Configured tally adapter used to launch this member.";
        pools = mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "Declared tally pools required by this member.";
        };
        launch = mkOption {
          type = launchType;
          default = { };
          description = "Typed adapter launch overrides for this member.";
        };
        architecture = nullableStringOption "Optional architecture label.";
        fineTune = nullableStringOption "Optional fine-tune lineage label.";
        backend = nullableStringOption "Optional inference backend label.";
        modality = nullableStringOption "Optional modality label.";
        role = nullableStringOption "Optional roster role.";
        status = nullableStringOption "Optional roster status.";
        evidence = nullableStringOption "Optional evidence tier.";
        hosts = mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "Optional physical host-placement labels.";
        };
        baseCheckpoint = nullableStringOption "Optional base-checkpoint identity.";
        supersedes = nullableStringOption "Optional predecessor member identity.";
        supersededBy = nullableStringOption "Optional successor member identity.";
        notes = nullableStringOption "Optional free-form roster notes.";
      };
    }
  );

  classType = types.submodule {
    options.diversity = mkOption {
      type = types.listOf (
        types.enum [
          "family"
          "maker"
        ]
      );
      default = [ ];
      example = [
        "family"
        "maker"
      ];
      description = "Diversity keys that every enabled member of this selector class must provide.";
    };
  };

  evalCatalog =
    {
      classes,
      pools,
      members,
    }:
    (lib.evalModules {
      modules = [
        {
          options = {
            classes = mkOption {
              type = types.attrsOf classType;
              description = "Closed selector-class registry for this catalog.";
            };
            pools = mkOption {
              type = types.listOf types.str;
              description = "Pool names declared by the consuming tally configuration.";
            };
            members = mkOption {
              type = types.attrsOf memberType;
              description = "Typed roster members rendered into catalog order.";
            };
          };
          config = {
            inherit classes pools members;
          };
        }
      ];
    }).config;

  enabledMembers = cfg: filterAttrs (_: member: member.enable) cfg.members;

  mkCatalogAssertions =
    cfg:
    let
      enabled = enabledMembers cfg;
      classNames = builtins.attrNames cfg.classes;
      optionalNonEmptyFields = [
        "architecture"
        "fineTune"
        "backend"
        "modality"
        "role"
        "status"
        "evidence"
        "baseCheckpoint"
        "supersedes"
        "supersededBy"
      ];
    in
    flatten [
      (mapAttrsToList (
        _: member:
        map (class: {
          assertion = builtins.hasAttr class cfg.classes;
          message = "tally catalog member ${member.id} references unknown class ${class}";
        }) member.classes
      ) enabled)
      (map (class: {
        assertion = lib.any (member: builtins.elem class member.classes) (builtins.attrValues enabled);
        message = "tally catalog class ${class} has no members after filtering";
      }) classNames)
      (mapAttrsToList (
        _: member:
        map (pool: {
          assertion = builtins.elem pool cfg.pools;
          message = "tally catalog member ${member.id} references undeclared pool ${pool}";
        }) member.pools
      ) enabled)
      (mapAttrsToList (
        class: classConfig:
        concatLists (
          mapAttrsToList (
            _: member:
            if builtins.elem class member.classes then
              map (
                key:
                let
                  value = member.${key};
                in
                {
                  assertion = value != null && nonEmptyString value;
                  message = "tally catalog class ${class} requires diversity key ${key}, but member ${member.id} does not define it";
                }
              ) classConfig.diversity
            else
              [ ]
          ) enabled
        )
      ) cfg.classes)
      [
        {
          assertion = builtins.length cfg.pools == builtins.length (unique cfg.pools);
          message = "tally catalog declared pools must be unique";
        }
      ]
      (map (pool: {
        assertion = nonEmptyString pool;
        message = "tally catalog declared pool names must be non-empty";
      }) cfg.pools)
      (mapAttrsToList (class: classConfig: [
        {
          assertion = nonEmptyString class;
          message = "tally catalog class names must be non-empty";
        }
        {
          assertion = builtins.length classConfig.diversity == builtins.length (unique classConfig.diversity);
          message = "tally catalog class ${class} diversity keys must be unique";
        }
      ]) cfg.classes)
      (mapAttrsToList (
        _: member:
        [
          {
            assertion = nonEmptyString member.id;
            message = "tally catalog member ids must be non-empty";
          }
          {
            assertion = member.family != null && nonEmptyString member.family;
            message = "tally catalog member ${member.id} family must be non-empty";
          }
          {
            assertion = member.maker != null && nonEmptyString member.maker;
            message = "tally catalog member ${member.id} maker must be non-empty";
          }
          {
            assertion =
              member.classes != [ ] && builtins.length member.classes == builtins.length (unique member.classes);
            message = "tally catalog member ${member.id} classes must contain unique non-empty values";
          }
          {
            assertion = builtins.all nonEmptyString member.classes;
            message = "tally catalog member ${member.id} classes must contain unique non-empty values";
          }
          {
            assertion = member.adapter != null && nonEmptyString member.adapter;
            message = "tally catalog member ${member.id} adapter must be non-empty";
          }
          {
            assertion =
              member.pools != [ ] && builtins.length member.pools == builtins.length (unique member.pools);
            message = "tally catalog member ${member.id} pools must contain unique non-empty values";
          }
          {
            assertion = builtins.all nonEmptyString member.pools;
            message = "tally catalog member ${member.id} pools must contain unique non-empty values";
          }
          {
            assertion = member.launch.model != null && nonEmptyString member.launch.model;
            message = "tally catalog member ${member.id} launch.model must be non-empty";
          }
          {
            assertion =
              member.hosts == [ ]
              || (
                builtins.length member.hosts == builtins.length (unique member.hosts)
                && builtins.all nonEmptyString member.hosts
              );
            message = "tally catalog member ${member.id} hosts must contain unique non-empty values";
          }
        ]
        ++ (map (
          field:
          let
            value = member.${field};
          in
          {
            assertion = value == null || nonEmptyString value;
            message = "tally catalog member ${member.id} ${field} must be null or non-empty";
          }
        ) optionalNonEmptyFields)
      ) enabled)
      [
        {
          assertion =
            let
              ids = map (member: member.id) (builtins.attrValues enabled);
            in
            builtins.length ids == builtins.length (unique ids);
          message = "tally catalog enabled member ids must be unique";
        }
      ]
    ];

  firstCatalogFailure = cfg: lib.findFirst (entry: !entry.assertion) null (mkCatalogAssertions cfg);

  renderLaunch =
    launch:
    {
      inherit (launch) model;
    }
    // optionalAttrs (launch.prePromptArgv != [ ]) { inherit (launch) prePromptArgv; }
    // optionalAttrs (launch.environment != { }) { inherit (launch) environment; }
    // optionalAttrs (launch.approvalPolicy != null) { inherit (launch) approvalPolicy; }
    // optionalAttrs (launch.sandboxPolicy != null) { inherit (launch) sandboxPolicy; }
    // optionalAttrs (launch.effort != null) { inherit (launch) effort; };

  renderMember =
    member:
    {
      inherit (member)
        id
        family
        maker
        classes
        adapter
        pools
        ;
      launch = renderLaunch member.launch;
    }
    // optionalAttrs (member.architecture != null) { inherit (member) architecture; }
    // optionalAttrs (member.fineTune != null) { inherit (member) fineTune; }
    // optionalAttrs (member.backend != null) { inherit (member) backend; }
    // optionalAttrs (member.modality != null) { inherit (member) modality; }
    // optionalAttrs (member.role != null) { inherit (member) role; }
    // optionalAttrs (member.status != null) { inherit (member) status; }
    // optionalAttrs (member.evidence != null) { inherit (member) evidence; }
    // optionalAttrs (member.hosts != [ ]) { inherit (member) hosts; }
    // optionalAttrs (member.baseCheckpoint != null) { inherit (member) baseCheckpoint; }
    // optionalAttrs (member.supersedes != null) { inherit (member) supersedes; }
    // optionalAttrs (member.supersededBy != null) { inherit (member) supersededBy; }
    // optionalAttrs (member.notes != null) { inherit (member) notes; };

  renderMembers =
    cfg:
    let
      enabled = enabledMembers cfg;
      names = lib.sort (
        leftName: rightName:
        let
          left = enabled.${leftName};
          right = enabled.${rightName};
        in
        if left.order == right.order then leftName < rightName else left.order < right.order
      ) (builtins.attrNames enabled);
    in
    map (name: renderMember enabled.${name}) names;

  mkCatalog =
    {
      pkgs,
      package ? self.packages.${pkgs.stdenv.hostPlatform.system}.tally,
      classes,
      pools,
      members,
    }:
    let
      cfg = evalCatalog {
        inherit classes pools members;
      };
      failure = firstCatalogFailure cfg;
      unchecked = pkgs.writeText "tally-flow-catalog-unchecked.json" (
        builtins.toJSON {
          version = 1;
          members = renderMembers cfg;
        }
        + "\n"
      );
      validationScript = pkgs.writeText "tally-flow-catalog-check.js" ''
        export const meta = ${
          builtins.toJSON {
            name = "catalog-renderer-check";
            description = "Validate a Nix-rendered tally selector catalog";
            pools = [ ];
            argsSchema = {
              type = "object";
            };
            selectors = builtins.attrNames cfg.classes;
          }
        };

        null;
      '';
    in
    assert lib.assertMsg (failure == null) (
      if failure == null then "tally catalog assertion failed" else failure.message
    );
    pkgs.runCommand "tally-flow-catalog.json"
      {
        nativeBuildInputs = [ package ];
      }
      ''
        ${lib.getExe package} flow check ${validationScript} --catalog ${unchecked} >/dev/null
        cp ${unchecked} "$out"
      '';
in
{
  inherit
    evalCatalog
    mkCatalog
    mkCatalogAssertions
    ;
}
