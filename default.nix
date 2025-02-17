{pkgs ? import <nixpkgs> {}}: let
  crane = pkgs.fetchzip {
    url = "https://github.com/ipetkov/crane/archive/bef2b45cd1273a9e621fb5292de89f4ed59ad812.tar.gz";
    sha256 = "1w5lw78j5x5p7al6kxyk48rd00821fws44s1050armzqnkdqiy54";
  };
  craneLib = import crane {inherit pkgs;};

  commonArgs = {
    src = craneLib.cleanCargoSource ./.;
    strictDeps = true;
  };
in
  craneLib.buildPackage commonArgs
  // {
    # Allow for reuse of previous dependency builds
    cargoArtifacts = craneLib.buildDepsOnly commonArgs;
    meta.mainProgram = "diski";
  }
