import java.io.File
import java.util.Properties
import javax.inject.Inject
import org.apache.tools.ant.taskdefs.condition.Os
import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.logging.LogLevel
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.TaskAction
import org.gradle.process.ExecOperations

open class BuildTask @Inject constructor(
    private val execOperations: ExecOperations
) : DefaultTask() {
    @Input
    var rootDirRel: String? = null
    @Input
    var target: String? = null
    @Input
    var release: Boolean? = null
    /** Android API level the NDK compilers target (the app's minSdk). */
    @Input
    var minSdk: Int = 24

    @TaskAction
    fun assemble() {
        // On Windows it runs through `cmd /c`, which finds npx.cmd itself. (Retrying as .exe,
        // .cmd and .bat only repeated a failed Rust build three more times.)
        runTauriCli("""npx""")
    }

    fun runTauriCli(executable: String) {
        val rootDirRel = rootDirRel ?: throw GradleException("rootDirRel cannot be null")
        val target = target ?: throw GradleException("target cannot be null")
        val release = release ?: throw GradleException("release cannot be null")
        val args = mutableListOf("tauri", "android", "android-studio-script")
        if (project.logger.isEnabled(LogLevel.DEBUG)) {
            args.add("-vv")
        } else if (project.logger.isEnabled(LogLevel.INFO)) {
            args.add("-v")
        }
        if (release) {
            args.add("--release")
        }
        args.addAll(listOf("--target", target))

        // Uses the normal CARGO_HOME (~/.cargo), like terminal builds. A project-local one makes
        // the plugin paths in tauri.settings.gradle flip between the two and rebuilds the Rust
        // dependencies on every switch. The Kotlin "different roots" error (registry on C:,
        // project on E:) is handled by kotlin.incremental=false in gradle.properties.
        val ndkEnv = ndkEnv(target)
        execOperations.exec {
            workingDir(File(project.projectDir, rootDirRel))
            environment(ndkEnv)
            if (Os.isFamily(Os.FAMILY_WINDOWS)) {
                executable("cmd.exe")
                args(listOf("/c", executable) + args)
            } else {
                executable(executable)
                args(args)
            }
        }.assertNormalExitValue()
    }

    /**
     * The NDK linker and C toolchain for a Rust target. Builds started from Android Studio don't
     * get them from the Tauri CLI, and cargo then fails with "linker `cc` not found" (the `ring`
     * and SQLite C code need the compilers too).
     */
    private fun ndkEnv(target: String): Map<String, String> {
        val ndk = findNdk() ?: run {
            logger.warn("Android NDK not found (set NDK_HOME, or install one with the SDK Manager)")
            return emptyMap()
        }
        val windows = Os.isFamily(Os.FAMILY_WINDOWS)
        val host = when {
            windows -> "windows-x86_64"
            Os.isFamily(Os.FAMILY_MAC) -> "darwin-x86_64"
            else -> "linux-x86_64"
        }
        val (triple, clangPrefix) = when (target) {
            "aarch64" -> "aarch64-linux-android" to "aarch64-linux-android"
            "armv7" -> "armv7-linux-androideabi" to "armv7a-linux-androideabi"
            "i686" -> "i686-linux-android" to "i686-linux-android"
            "x86_64" -> "x86_64-linux-android" to "x86_64-linux-android"
            else -> return emptyMap()
        }
        val bin = File(ndk, "toolchains/llvm/prebuilt/$host/bin")
        val script = if (windows) ".cmd" else ""
        val cc = File(bin, "$clangPrefix$minSdk-clang$script").path
        val cxx = File(bin, "$clangPrefix$minSdk-clang++$script").path
        val ar = File(bin, "llvm-ar" + if (windows) ".exe" else "").path
        val underscored = triple.replace('-', '_')
        return mapOf(
            "CARGO_TARGET_${underscored.uppercase()}_LINKER" to cc,
            "CC_$underscored" to cc,
            "CXX_$underscored" to cxx,
            "AR_$underscored" to ar,
        )
    }

    /** NDK_HOME, else the newest NDK in the Android SDK (as the Tauri CLI picks it). */
    private fun findNdk(): File? {
        System.getenv("NDK_HOME")?.let(::File)?.takeIf { it.isDirectory }?.let { return it }
        val sdk = System.getenv("ANDROID_HOME")
            ?: System.getenv("ANDROID_SDK_ROOT")
            ?: File(project.rootDir, "local.properties").takeIf { it.isFile }?.let { f ->
                Properties().apply { f.inputStream().use { load(it) } }.getProperty("sdk.dir")
            }
            ?: return null
        return File(sdk, "ndk").listFiles { f -> f.isDirectory }
            ?.maxWithOrNull { a, b -> compareVersions(a.name, b.name) }
    }

    private fun compareVersions(a: String, b: String): Int {
        val x = a.split('.').map { it.toIntOrNull() ?: 0 }
        val y = b.split('.').map { it.toIntOrNull() ?: 0 }
        for (i in 0 until maxOf(x.size, y.size)) {
            val d = x.getOrElse(i) { 0 } - y.getOrElse(i) { 0 }
            if (d != 0) return d
        }
        return 0
    }
}
