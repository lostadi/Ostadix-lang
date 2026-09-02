package org.ostadix.terminal;

import android.content.Context;
import android.content.res.AssetManager;
import android.system.ErrnoException;
import android.system.Os;
import android.system.OsConstants;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/** Installs versioned, bundled Ostadix assets into this APK's private home. */
public final class AppFiles {
    private static final String TERMUX_HOME = "/data/data/com.termux/files/home";
    private static final String TERMUX_PREFIX = "/data/data/com.termux/files/usr";
    private static final String TERMUX_CANONICAL_HOME =
            "/data/user/0/com.termux/files/home";
    private static final String ASSET_VERSION = "ostadix-terminal-assets-v2";
    private static final String CLI_LIBRARY_NAME = "libostadix_cli.so";
    private static final String BASH_LIBRARY_NAME = "libostadix_bash.so";
    private static final String[] BASH_RUNTIME_LIBRARIES = {
            BASH_LIBRARY_NAME,
            "libandroid-support.so",
            "libiconv.so",
            "libreadline_8.so",
            "libncursesw_6.so"
    };

    private final Context context;
    private final File nativeLibraries;
    private final File home;
    private final File bin;
    private final File workspace;
    private final File backends;
    private final File terminfo;
    private final File inputrc;
    private String bashInstallError;

    public AppFiles(Context context) {
        this.context = context.getApplicationContext();
        String nativeLibraryDirectory = this.context.getApplicationInfo().nativeLibraryDir;
        this.nativeLibraries = nativeLibraryDirectory == null
                ? null
                : new File(nativeLibraryDirectory);
        this.home = new File(this.context.getFilesDir(), "home");
        this.bin = new File(home, "bin");
        this.workspace = new File(home, "workspace");
        this.backends = new File(home, "backends");
        this.terminfo = new File(home, "terminfo");
        this.inputrc = new File(home, ".inputrc");
    }

    public void install() throws IOException {
        mkdir(home);
        mkdir(bin);
        mkdir(workspace);
        installCommandLink(cliCommand(), CLI_LIBRARY_NAME, "O CLI");
        try {
            validateBashRuntime();
            installCommandLink(bashCommand(), BASH_LIBRARY_NAME, "Bash");
            bashInstallError = null;
        } catch (IOException error) {
            // Ostadix still has a safe Android-system-shell fallback. The APK
            // build treats a missing Bash closure as fatal, so this path is
            // reserved for a damaged or unusually extracted installation.
            bashInstallError = error.getMessage();
        }
        File marker = new File(home, ".assets-version");
        String installed = marker.isFile()
                ? readSmallFile(marker).trim()
                : "";
        boolean assetsChanged = !ASSET_VERSION.equals(installed);
        if (assetsChanged) {
            copyAssetTree("backends", backends);
            copyAssetTree("examples", workspace);
        }
        if (assetsChanged || !new File(terminfo, "x/xterm-256color").isFile()) {
            copyAssetTree("terminfo", terminfo);
        }
        if (!inputrc.isFile()) {
            copyAssetTree("shell/inputrc", inputrc);
        }
        if (assetsChanged) {
            writeAtomic(marker, ASSET_VERSION + "\n");
        }
    }

    public File home() {
        return home;
    }

    public File workspace() {
        return workspace;
    }

    public File bin() {
        return bin;
    }

    public File cliCommand() {
        return new File(bin, "O");
    }

    public File bashCommand() {
        return new File(bin, "bash");
    }

    public boolean isBashAvailable() {
        if (bashInstallError != null || !bashCommand().canExecute()) {
            return false;
        }
        try {
            validateBashRuntime();
            return true;
        } catch (IOException ignored) {
            return false;
        }
    }

    public String bashUnavailableReason() {
        if (bashInstallError != null) {
            return bashInstallError;
        }
        return "bundled Bash or one of its native libraries is unavailable";
    }

    public File backends() {
        return backends;
    }

    public File helloExample() {
        return new File(workspace, "hello.O");
    }

    /** Environment for the sandboxed Bash or Android-sh fallback session. */
    public String[] nonRootEnvironment(boolean bundledBash) {
        List<String> environment = new ArrayList<>();
        environment.add("HOME=" + home.getAbsolutePath());
        environment.add("TMPDIR=" + context.getCacheDir().getAbsolutePath());
        environment.add("PATH=" + bin.getAbsolutePath() + ":/system/bin:/system/xbin");
        environment.add("SHELL=" + (bundledBash
                ? bashCommand().getAbsolutePath()
                : "/system/bin/sh"));
        environment.add("PWD=" + workspace.getAbsolutePath());
        environment.add("PS1=ostadix:\\w $ ");
        environment.add("TERM=xterm-256color");
        environment.add("COLORTERM=truecolor");
        environment.add("LANG=C.UTF-8");
        environment.add("ANDROID_ROOT=/system");
        environment.add("ANDROID_DATA=/data");
        environment.add("OSTADIX_HOME=" + home.getAbsolutePath());
        environment.add("O_BACKENDS_DIR=" + backends.getAbsolutePath());
        if (bundledBash) {
            // Applied only to the non-root Bash child. Some Android linker
            // namespaces ignore the executable's valid ${ORIGIN} RUNPATH;
            // this sanitized path is the tested fallback and never reaches su.
            environment.add("LD_LIBRARY_PATH=" + nativeLibraries.getAbsolutePath());
            environment.add("TERMINFO=" + terminfo.getAbsolutePath());
            environment.add("INPUTRC=" + inputrc.getAbsolutePath());
        }
        return environment.toArray(new String[environment.size()]);
    }

    /**
     * Minimal environment for the explicit KernelSU session.
     *
     * <p>Every value is independent of app-writable storage. The PTY child
     * enters read-only {@code /system} before executing su, so HOME and PWD
     * describe the real initial directory and cannot contain app-written
     * startup files. An explicitly empty ENV prevents KernelSU from injecting
     * a startup hook, and no temporary directory is imposed on privileged tools.
     */
    public String[] rootEnvironment() {
        return new String[] {
                "HOME=/system",
                "USER=root",
                "LOGNAME=root",
                "ENV=",
                "PATH=/system/bin:/system/xbin",
                "SHELL=/system/bin/sh",
                "PWD=/system",
                "TERM=xterm-256color",
                "COLORTERM=truecolor",
                "LANG=C.UTF-8",
                "ANDROID_ROOT=/system",
                "ANDROID_DATA=/data"
        };
    }

    /** Environment installed after KernelSU grants access to Termux's private tree. */
    public String[] termuxEnvironment() {
        return new String[] {
                "HOME=" + TERMUX_HOME,
                "PREFIX=" + TERMUX_PREFIX,
                "TERMUX__HOME=" + TERMUX_HOME,
                "TERMUX__PREFIX=" + TERMUX_PREFIX,
                "TERMUX__ROOTFS_DIR=/data/data/com.termux/files",
                "TMPDIR=" + TERMUX_PREFIX + "/tmp",
                "PATH=" + TERMUX_PREFIX + "/bin:"
                        + TERMUX_HOME + "/.local/bin:"
                        + TERMUX_HOME + "/.cargo/bin:"
                        + TERMUX_HOME + "/bin:"
                        + TERMUX_HOME + "/Ostadix-lang/target/release:"
                        + "/system/bin:/system/xbin",
                "SHELL=" + TERMUX_PREFIX + "/bin/zsh",
                "PWD=" + TERMUX_HOME,
                "TERM=xterm-256color",
                "COLORTERM=truecolor",
                "LANG=C.UTF-8",
                "LC_ALL=C.UTF-8",
                "ANDROID_ROOT=/system",
                "ANDROID_DATA=/data",
                "SSL_CERT_DIR=" + TERMUX_PREFIX + "/etc/tls",
                "SSL_CERT_FILE=" + TERMUX_PREFIX + "/etc/tls/cert.pem",
                "O_LANG_ROOT=" + TERMUX_HOME + "/Ostadix-lang",
                "O_BACKENDS_DIR=" + TERMUX_HOME + "/Ostadix-lang/backends",
                "OSTADIX_HOME=" + TERMUX_HOME,
                "OSTADIX_GUESTS_DIR=" + TERMUX_HOME + "/.local/share/ostadix/guests"
        };
    }

    public String termuxLoginCommand() {
        // Do not expose LD_PRELOAD to su itself: it cannot traverse Termux's
        // private directory until after KernelSU elevates the process.
        // /data/user/0 is Android's canonical package-data tree. The
        // /data/data alias can be filtered from an app mount namespace even
        // after the UID changes, so use the canonical path for traversal.
        return "cd " + TERMUX_CANONICAL_HOME
                + " && export LD_PRELOAD=" + TERMUX_PREFIX
                + "/lib/libtermux-exec-ld-preload.so"
                + " && exec " + TERMUX_PREFIX + "/bin/zsh -l";
    }

    /**
     * Exposes the APK-bundled CLI without copying executable code into the
     * writable app home. Android's package manager owns and labels the target
     * in nativeLibraryDir; HOME/bin contains only a private symbolic link.
     */
    private void installCommandLink(
            File command,
            String libraryName,
            String displayName
    ) throws IOException {
        if (nativeLibraries == null) {
            throw new IOException("Android did not provide a native library directory");
        }
        File nativeCommand = new File(nativeLibraries, libraryName);
        if (!nativeCommand.isFile() || !nativeCommand.canExecute()) {
            throw new IOException(
                    "Bundled " + displayName + " is missing or not executable: " + nativeCommand);
        }

        String commandPath = command.getAbsolutePath();
        String targetPath = nativeCommand.getAbsolutePath();
        try {
            String currentTarget = Os.readlink(commandPath);
            if (targetPath.equals(currentTarget)) {
                return;
            }
            Os.remove(commandPath);
        } catch (ErrnoException error) {
            if (error.errno == OsConstants.EINVAL) {
                // The reserved command path is a regular file from an older
                // build. Remove it; never execute that writable copy.
                if (!command.delete()) {
                    throw new IOException(
                            "Unable to replace writable " + displayName + " command", error);
                }
            } else if (error.errno != OsConstants.ENOENT) {
                throw new IOException("Unable to inspect " + displayName + " command link", error);
            }
        }

        try {
            Os.symlink(targetPath, commandPath);
        } catch (ErrnoException error) {
            // A concurrent Activity launch may have created the same safe link.
            try {
                if (targetPath.equals(Os.readlink(commandPath))) {
                    return;
                }
            } catch (ErrnoException ignored) {
                // Report the original, actionable creation failure below.
            }
            throw new IOException("Unable to expose the bundled " + displayName + " command", error);
        }
    }

    private void validateBashRuntime() throws IOException {
        if (nativeLibraries == null) {
            throw new IOException("Android did not provide a native library directory");
        }
        for (String libraryName : BASH_RUNTIME_LIBRARIES) {
            File library = new File(nativeLibraries, libraryName);
            if (!library.isFile() || !library.canRead()) {
                throw new IOException("Bundled Bash dependency is missing: " + library);
            }
        }
    }

    private void copyAssetTree(String assetPath, File destination) throws IOException {
        AssetManager assets = context.getAssets();
        String[] children = assets.list(assetPath);
        if (children != null && children.length > 0) {
            mkdir(destination);
            for (String child : children) {
                copyAssetTree(assetPath + "/" + child, new File(destination, child));
            }
            return;
        }

        File parent = destination.getParentFile();
        if (parent != null) {
            mkdir(parent);
        }
        File temporary = new File(parent, destination.getName() + ".new");
        try (InputStream input = assets.open(assetPath);
             FileOutputStream output = new FileOutputStream(temporary)) {
            byte[] buffer = new byte[32 * 1024];
            int read;
            while ((read = input.read(buffer)) != -1) {
                output.write(buffer, 0, read);
            }
            output.getFD().sync();
        }
        if (destination.exists() && !destination.delete()) {
            throw new IOException("Unable to replace asset " + assetPath);
        }
        if (!temporary.renameTo(destination)) {
            throw new IOException("Unable to install asset " + assetPath);
        }
    }

    private static void writeAtomic(File destination, String value) throws IOException {
        File temporary = new File(destination.getParentFile(), destination.getName() + ".new");
        try (FileOutputStream output = new FileOutputStream(temporary)) {
            output.write(value.getBytes(StandardCharsets.UTF_8));
            output.getFD().sync();
        }
        if (destination.exists() && !destination.delete()) {
            throw new IOException("Unable to replace " + destination);
        }
        if (!temporary.renameTo(destination)) {
            throw new IOException("Unable to write " + destination);
        }
    }

    private static String readSmallFile(File file) throws IOException {
        byte[] data = new byte[(int) Math.min(file.length(), 4096)];
        int offset = 0;
        try (FileInputStream input = new FileInputStream(file)) {
            while (offset < data.length) {
                int read = input.read(data, offset, data.length - offset);
                if (read < 0) {
                    break;
                }
                offset += read;
            }
        }
        return new String(data, 0, offset, StandardCharsets.UTF_8);
    }

    private static void mkdir(File directory) throws IOException {
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IOException("Unable to create " + directory);
        }
    }
}
