{
  lib,
  pkgs,
  tallyPackage,
}:

pkgs.writeShellApplication {
  name = "tally-witness-emit";
  runtimeInputs = [ pkgs.jq ];
  text = ''
    if [[ "$#" -ne 1 ]]; then
      echo "usage: tally-witness-emit JSON_OR_OUTCOME:UNIT" >&2
      exit 2
    fi
    payload="$1"
    case "$payload" in
      success:*|failure:*)
        outcome="''${payload%%:*}"
        unit="''${payload#*:}"
        payload="$(jq -cn --arg kind systemd-unit --arg outcome "$outcome" --arg unit "$unit" '{kind: $kind, outcome: $outcome, unit: $unit}')"
        ;;
    esac

    if [[ -n "''${TALLY_ATTESTATION_LEDGER:-}" ]]; then
      ledger="$TALLY_ATTESTATION_LEDGER"
    elif [[ -n "''${XDG_DATA_HOME:-}" ]]; then
      ledger="$XDG_DATA_HOME/tally/attestations.jsonl"
    else
      ledger="''${HOME:?HOME or XDG_DATA_HOME is required}/.local/share/tally/attestations.jsonl"
    fi

    exec ${lib.getExe tallyPackage} witness append --ledger "$ledger" --payload "$payload"
  '';
}
