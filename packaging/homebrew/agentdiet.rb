# Homebrew formula TEMPLATE for AgentDiet. Not published anywhere yet.
#
# To publish (see docs/RELEASE_READINESS.md, "Publishing"): after the GitHub release for VERSION is out,
# replace VERSION and the four SHA256 placeholders with the values from that release's SHA256SUMS,
# then add this file to a tap (e.g. github.com/Rohitrajsaji/homebrew-tap as Formula/agentdiet.rb).
class Agentdiet < Formula
  desc "Context firewall for AI coding agents: trims noisy tool output, keeps it recoverable"
  homepage "https://github.com/Rohitrajsaji/agentdiet"
  version "VERSION"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/Rohitrajsaji/agentdiet/releases/download/vVERSION/agentdiet-VERSION-aarch64-apple-darwin.tar.gz"
      sha256 "SHA256_AARCH64_APPLE_DARWIN"
    end
    on_intel do
      url "https://github.com/Rohitrajsaji/agentdiet/releases/download/vVERSION/agentdiet-VERSION-x86_64-apple-darwin.tar.gz"
      sha256 "SHA256_X86_64_APPLE_DARWIN"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Rohitrajsaji/agentdiet/releases/download/vVERSION/agentdiet-VERSION-aarch64-unknown-linux-musl.tar.gz"
      sha256 "SHA256_AARCH64_UNKNOWN_LINUX_MUSL"
    end
    on_intel do
      url "https://github.com/Rohitrajsaji/agentdiet/releases/download/vVERSION/agentdiet-VERSION-x86_64-unknown-linux-musl.tar.gz"
      sha256 "SHA256_X86_64_UNKNOWN_LINUX_MUSL"
    end
  end

  def install
    bin.install "agentdiet"
    generate_completions_from_executable(bin/"agentdiet", "completions")
  end

  def caveats
    <<~EOS
      Connect your assistants:
        agentdiet init
      Assistant configs point at #{HOMEBREW_PREFIX}/bin/agentdiet, which stays valid across upgrades.
      Before `brew uninstall agentdiet`, run `agentdiet eject` to restore your original configs.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/agentdiet --version")
    ENV["HOME"] = testpath
    system bin/"agentdiet", "status", "--json"
  end
end
