{
  description = "dfdu: disk usage browser for the terminal";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "dfdu";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "-p" "dfdu" ];
          # The test suite needs a terminal, FUSE and root for some cases; CI runs it.
          doCheck = false;
          nativeBuildInputs = [ pkgs.installShellFiles ];
          postInstall = ''
            installManPage assets/man/*.1
            installShellCompletion --bash --name dfdu assets/completions/dfdu.bash
            installShellCompletion --zsh assets/completions/_dfdu
            installShellCompletion --fish assets/completions/dfdu.fish
          '';
          meta = {
            description = "Disk usage browser for the terminal";
            homepage = "https://github.com/jordond/dfdu";
            license = pkgs.lib.licenses.mit;
            mainProgram = "dfdu";
          };
        };
      });
    };
}
