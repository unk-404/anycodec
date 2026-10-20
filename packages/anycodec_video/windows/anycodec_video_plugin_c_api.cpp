#include "include/anycodec_video/anycodec_video_plugin_c_api.h"

#include <flutter/plugin_registrar_windows.h>

#include "anycodec_video_plugin.h"

void AnycodecVideoPluginCApiRegisterWithRegistrar(
    FlutterDesktopPluginRegistrarRef registrar) {
  anycodec_video::AnycodecVideoPlugin::RegisterWithRegistrar(
      flutter::PluginRegistrarManager::GetInstance()
          ->GetRegistrar<flutter::PluginRegistrarWindows>(registrar));
}
