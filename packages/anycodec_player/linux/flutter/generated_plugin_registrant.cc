//
//  Generated file. Do not edit.
//

// clang-format off

#include "generated_plugin_registrant.h"

#include <anycodec_video/anycodec_video_plugin.h>

void fl_register_plugins(FlPluginRegistry* registry) {
  g_autoptr(FlPluginRegistrar) anycodec_video_registrar =
      fl_plugin_registry_get_registrar_for_plugin(registry, "AnycodecVideoPlugin");
  anycodec_video_plugin_register_with_registrar(anycodec_video_registrar);
}
