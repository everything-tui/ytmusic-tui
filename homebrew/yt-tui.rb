# Sketch Homebrew formula — publish release assets, then fill in sha256 + version.
class YtTui < Formula
  desc "YouTube Music in the terminal"
  homepage "https://github.com/francoisegner/yt-tui"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/francoisegner/yt-tui/releases/download/v#{version}/yt-tui-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_ME"
    end
    on_intel do
      url "https://github.com/francoisegner/yt-tui/releases/download/v#{version}/yt-tui-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_ME"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/francoisegner/yt-tui/releases/download/v#{version}/yt-tui-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_ME"
    end
    on_intel do
      url "https://github.com/francoisegner/yt-tui/releases/download/v#{version}/yt-tui-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_ME"
    end
  end

  def install
    bin.install "yt-tui"
  end

  test do
    assert_match "yt-tui", shell_output("#{bin}/yt-tui --help")
  end
end
