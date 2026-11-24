package com.plugin.updaterplus

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.activity.result.ActivityResult
import androidx.core.content.FileProvider
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import kotlin.concurrent.thread

@InvokeArg
class InstallApkArgs {
    lateinit var path: String
}

@TauriPlugin
class UpdaterPlusPlugin(private val activity: Activity) : Plugin(activity) {
    @Volatile
    private var pendingApkFile: File? = null

    @Command
    fun getBaseApkPath(invoke: Invoke) {
        val result = JSObject()
        result.put("path", activity.applicationInfo?.sourceDir)
        invoke.resolve(result)
    }

    @Command
    fun installApk(invoke: Invoke) {
        val args = try {
            invoke.parseArgs(InstallApkArgs::class.java)
        } catch (error: Exception) {
            invoke.reject("Invalid APK install arguments: ${error.message}")
            return
        }

        thread(name = "updater-plus-stage-apk") {
            try {
                val source = File(args.path)
                if (!source.isFile || !source.canRead()) {
                    invoke.reject("APK file not found or unreadable: ${args.path}")
                    return@thread
                }

                val stagedApk = stageApkForFileProvider(source)
                activity.runOnUiThread {
                    try {
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
                            !activity.packageManager.canRequestPackageInstalls()
                        ) {
                            pendingApkFile = stagedApk
                            val intent = Intent(
                                Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                                Uri.parse("package:${activity.packageName}"),
                            )
                            startActivityForResult(invoke, intent, "onUnknownSourcesResult")
                            return@runOnUiThread
                        }

                        launchApkInstall(stagedApk)
                        invoke.resolve(JSObject())
                    } catch (error: Exception) {
                        invoke.reject("Failed to launch Android APK installer: ${error.message}")
                    }
                }
            } catch (error: Exception) {
                invoke.reject("Failed to stage Android APK: ${error.message}")
            }
        }
    }

    @ActivityCallback
    fun onUnknownSourcesResult(invoke: Invoke, result: ActivityResult) {
        val _ignored = result
        val stagedApk = pendingApkFile
        pendingApkFile = null
        if (stagedApk == null || !stagedApk.isFile) {
            invoke.reject("Pending APK update file is no longer available")
            return
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
            !activity.packageManager.canRequestPackageInstalls()
        ) {
            invoke.reject("Permission to install unknown apps was not granted")
            return
        }
        try {
            launchApkInstall(stagedApk)
            invoke.resolve(JSObject())
        } catch (error: Exception) {
            invoke.reject("Failed to launch Android APK installer: ${error.message}")
        }
    }

    private fun stageApkForFileProvider(source: File): File {
        val cacheDir = File(activity.cacheDir, "updater-plus")
        if (!cacheDir.exists() && !cacheDir.mkdirs()) {
            throw IllegalStateException("Failed to create cache directory: ${cacheDir.absolutePath}")
        }
        val target = File(cacheDir, "pending-update.apk")
        if (source.canonicalFile == target.canonicalFile) {
            return target
        }
        val temp = File(cacheDir, "pending-update.apk.tmp")
        source.copyTo(temp, overwrite = true)
        if (target.exists() && !target.delete()) {
            temp.delete()
            throw IllegalStateException("Failed to replace previous staged APK")
        }
        if (!temp.renameTo(target)) {
            temp.copyTo(target, overwrite = true)
            temp.delete()
        }
        return target
    }

    private fun launchApkInstall(apkFile: File) {
        val authority = "${activity.packageName}.updaterplus.fileprovider"
        val uri = FileProvider.getUriForFile(activity, authority, apkFile)
        val intent = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, "application/vnd.android.package-archive")
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        activity.startActivity(intent)
    }
}
