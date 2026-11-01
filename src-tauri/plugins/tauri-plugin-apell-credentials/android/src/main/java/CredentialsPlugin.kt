package dev.apell.credentials

import android.app.Activity
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

@InvokeArg
class CredentialArgs {
  lateinit var account: String
  var value: String? = null
}

@TauriPlugin
class CredentialsPlugin(private val activity: Activity) : Plugin(activity) {
  private fun key(): SecretKey {
    val alias = "apell_credentials_v1"
    val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (store.getKey(alias, null) as? SecretKey)?.let { return it }
    return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
      init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
        .setRandomizedEncryptionRequired(true).build())
      generateKey()
    }
  }

  private fun file(account: String): File {
    require(account.matches(Regex("[a-f0-9]{64}")))
    // Excluded from device backup; ciphertext and keystore key have the same lifetime.
    return File(activity.noBackupFilesDir, "credential_$account")
  }

  @Command
  fun read(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(CredentialArgs::class.java)
      val file = file(args.account)
      val response = JSObject()
      if (file.exists()) {
        require(file.length() <= 256 * 1024)
        val parts = file.readText().split(":")
        require(parts.size == 2)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, Base64.decode(parts[0], Base64.NO_WRAP)))
        cipher.updateAAD(args.account.toByteArray(Charsets.UTF_8))
        response.put("value", String(cipher.doFinal(Base64.decode(parts[1], Base64.NO_WRAP)), Charsets.UTF_8))
      } else {
        response.put("value", org.json.JSONObject.NULL)
      }
      invoke.resolve(response)
    } catch (_: Exception) { invoke.reject("Could not read the secure credential.") }
  }

  @Command
  fun write(invoke: Invoke) {
    try {
      val args = invoke.parseArgs(CredentialArgs::class.java)
      val file = file(args.account)
      val value = args.value
      if (value == null) {
        require(!file.exists() || file.delete())
      } else {
        require(value.toByteArray(Charsets.UTF_8).size <= 128 * 1024)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key())
        cipher.updateAAD(args.account.toByteArray(Charsets.UTF_8))
        val encrypted = cipher.doFinal(value.toByteArray(Charsets.UTF_8))
        val temporary = File(file.parentFile, "${file.name}.tmp")
        temporary.outputStream().use { stream ->
          stream.write((Base64.encodeToString(cipher.iv, Base64.NO_WRAP) + ":" +
            Base64.encodeToString(encrypted, Base64.NO_WRAP)).toByteArray(Charsets.UTF_8))
          stream.fd.sync()
        }
        require(temporary.renameTo(file))
      }
      invoke.resolve(JSObject())
    } catch (_: Exception) { invoke.reject("Could not save the secure credential.") }
  }
}
