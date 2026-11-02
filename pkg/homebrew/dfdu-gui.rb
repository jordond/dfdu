# The Homebrew cask of the GUI, filled per release by .github/scripts/brew-cask.sh and pushed
# to jordond/homebrew-tap by the gui-release workflow.
cask "dfdu-gui" do
  arch arm: "aarch64", intel: "x86_64"

  version "@VERSION@"
  sha256 arm:   "@SHA256_aarch64-apple-darwin.dmg@",
         intel: "@SHA256_x86_64-apple-darwin.dmg@"

  url "https://github.com/jordond/dfdu/releases/download/v#{version}/dfdu-gui-#{arch}-apple-darwin.dmg"
  name "dfdu"
  desc "Disk usage browser"
  homepage "https://dfdu.app/"

  depends_on macos: ">= :ventura"

  app "dfdu.app"

  caveats <<~EOS
    To scan every folder as an administrator, open View > Full disk access in dfdu. It
    installs a helper that macOS asks you to allow in System Settings > General > Login
    Items. Remove the helper in that window before uninstalling the app.
  EOS
end
