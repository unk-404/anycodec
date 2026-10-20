// Build hook for the AnyCodec native library.
//
// This file must be self-contained: `hooks_runner` only ever looks at
// `hook/build.dart` (and `hook/link.dart`), so there is no `hook/src/`.
//
// `native_toolchain_rust` compiles `rust/` into a `cdylib` and hands it to
// Flutter/Dart as a code asset. The asset name below is not arbitrary: it is the
// `lib/`-relative path of the file that carries `@DefaultAsset(...)`, which is
// the ffigen-generated binding. `package:anycodec/<assetName>` is the asset id
// that `@ffi.Native` resolves against.
//
// ## Why this hook re-executes itself
//
// `hooks_runner` runs every build hook with a *filtered* environment: only a
// whitelist (`PATH`, `LIBCLANG_PATH`, `TEMP`, ...) is forwarded. That list does
// **not** include `CARGO_HOME`, `RUSTUP_HOME`, or any of the variables a Visual
// Studio "Developer Command Prompt" exports (`INCLUDE`, `LIB`, `VCINSTALLDIR`,
// ...).
//
// The consequences are real:
//
//   * `rustup` cannot see a relocated `CARGO_HOME`/`RUSTUP_HOME`, so it would
//     download a second, several-gigabyte copy of the pinned toolchain into
//     `%USERPROFILE%\.rustup`;
//   * `rustc` cannot see `INCLUDE`/`LIB`, so linking fails — even when the
//     developer is correctly set up — with the famously unhelpful
//     "linker `link.exe` not found".
//
// `Platform.environment` is an unmodifiable snapshot and a build hook cannot set
// variables for its children, so the only way to hand these through is to run
// *ourselves* again with them added. The child sees the marker and performs the
// build, so both the `rustup` and the `cargo` invocation see the developer's own
// setup.
//
// Crucially this hook **does not guess**. It never invents a toolchain location:
// every value comes from the inherited environment, from an explicit
// `hooks: user_defines:` override, or from the value the developer registered
// for their own Windows profile. Carrying what the machine already has — and
// doing nothing surprising on a machine that has nothing — is what makes a hook
// publishable.
//
// ## Overrides
//
// ```yaml
// # pubspec.yaml of the application that depends on anycodec
// hooks:
//   user_defines:
//     anycodec:
//       cargo_home: D:\Devel\Rust\Cargo
//       rustup_home: D:\Devel\Rust\Rustup
//       include: "C:\...\VC\Tools\MSVC\...\include"
//       lib: "C:\...\VC\Tools\MSVC\...\lib\x64"
//       path: 'C:\...\VC\Tools\MSVC\...\bin\Hostx64\x64'
// ```
//
// To build the Rust crate with debug optimisations:
//   ANYCODEC_RUST_DEBUG=1 flutter run
import 'dart:convert';
import 'dart:io';

import 'package:code_assets/code_assets.dart';
import 'package:hooks/hooks.dart';
import 'package:logging/logging.dart';
import 'package:native_toolchain_rust/native_toolchain_rust.dart';

/// The code asset name: `lib/src/bindings/anycodec_bindings_generated.dart`.
const String _assetName = 'src/bindings/anycodec_bindings_generated.dart';

/// The directory holding the Cargo package, relative to the package root.
const String _cratePath = 'rust';

/// Marker set on the re-executed child so it never re-executes again.
const String _reexecMarker = 'ANYCODEC_HOOK_REEXEC';

/// The key this package reads out of `hooks: user_defines:`.
const String _packageDefinesKey = 'anycodec';

/// Environment variables `hooks_runner` filters out but the Rust build needs.
const List<String> _propagatedVariables = [
  'CARGO_HOME',
  'RUSTUP_HOME',
  'VCINSTALLDIR',
  'VSINSTALLDIR',
  'VCToolsInstallDir',
  'VCToolsVersion',
  'WindowsSdkDir',
  'WindowsSDKVersion',
  'UniversalCRTSdkDir',
  'UCRTVersion',
  'INCLUDE',
  'LIB',
  'LIBPATH',
];

Future<void> main(List<String> args) async {
  final overrides = _anycodecUserDefines(args);
  final logger = _createLogger();

  // Step 1: work out which variables this hook is missing but should have. On
  // Windows that includes the Visual Studio developer environment, obtained
  // through Microsoft's documented `Enter-VsDevShell` entry point.
  final propagated = await _collectPropagatedEnvironment(overrides, logger);

  // Step 2: if there is anything to add, run ourselves again with it present.
  // The child sees the marker and skips straight to the build.
  if (propagated.isNotEmpty && Platform.environment[_reexecMarker] == null) {
    final exitCode = await _reexecWithEnvironment(args, propagated);
    if (exitCode != null) {
      exit(exitCode);
    }
    // Re-execution was impossible; fall through and build with what we have.
  }

  await build(args, (input, output) async {
    // Not every target wants a code asset — the web build does not — and
    // `input.config.code` *throws* when code assets are not being built
    // ("HookConfig.code should only be accessed when building code assets").
    // The guard left `package:hooks` in 2.2.0; it now lives in
    // `package:code_assets`, which is already a dependency.
    if (!input.config.buildCodeAssets) {
      logger.info('no code assets requested for this target; nothing to build');
      return;
    }

    final codeConfig = input.config.code;
    logger.info(
      'Building the AnyCodec code asset for '
      '${codeConfig.targetOS.name}/${codeConfig.targetArchitecture.name} '
      '(link mode preference: ${codeConfig.linkModePreference})',
    );

    final environment = <String, String>{
      ...propagated,
      ..._privateTempEnvironment(input, logger),
    };

    if (codeConfig.targetOS == OS.windows &&
        !_msvcToolchainIsAvailable(environment)) {
      _warnAboutMissingMsvcToolchain(logger);
    }

    await RustBuilder(
      assetName: _assetName,
      cratePath: _cratePath,
      buildMode: Platform.environment['ANYCODEC_RUST_DEBUG'] == '1'
          ? BuildMode.debug
          : BuildMode.release,
      extraCargoEnvironmentVariables: environment,
    ).run(input: input, output: output, logger: logger);
  });
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// A detached logger writing `[anycodec] ...` lines to the hook's stdio.
///
/// `Logger.detached` is required: `package:logging` refuses `level =` on an
/// attached non-root logger unless the whole hierarchy is enabled, and a build
/// hook has no business reconfiguring the logging of the SDK it runs inside.
Logger _createLogger() {
  return Logger.detached('anycodec/build')
    ..level = Level.ALL
    ..onRecord.listen((record) {
      final sink = record.level >= Level.WARNING ? stderr : stdout;
      sink.writeln('[anycodec] ${record.message}');
      final error = record.error;
      if (error != null) {
        sink.writeln('[anycodec]   $error');
      }
    });
}

// ---------------------------------------------------------------------------
// Environment propagation
// ---------------------------------------------------------------------------

/// Reads `hooks: user_defines: anycodec:` out of the hook's `--config` argument.
Map<String, Object?> _anycodecUserDefines(List<String> args) {
  for (final arg in args) {
    if (!arg.startsWith('-config=')) {
      continue;
    }
    final file = File(arg.substring('-config='.length));
    if (!file.existsSync()) {
      return const {};
    }
    try {
      final decoded = jsonDecode(file.readAsStringSync());
      if (decoded is! Map) {
        return const {};
      }
      final defines = decoded['user_defines'];
      if (defines is! Map) {
        return const {};
      }
      final packageDefines = defines[_packageDefinesKey];
      if (packageDefines is! Map) {
        return const {};
      }
      return packageDefines.cast<String, Object?>();
    } on Object {
      return const {};
    }
  }
  return const {};
}

/// Collects every variable this hook should add before building.
///
/// Two sources, in order:
///
/// 1. [_resolveInheritedEnvironment] for whatever the hook environment is
///    missing but the developer's own profile already has.
/// 2. On Windows, optionally, the Visual Studio developer environment obtained
///    through Microsoft's documented `Enter-VsDevShell` entry point. That is
///    opt-in (`hooks: user_defines: anycodec: msvc_devshell: true`) because it
///    costs a PowerShell launch, and because `rustc` finds Visual Studio by
///    itself in the overwhelmingly common case.
Future<Map<String, String>> _collectPropagatedEnvironment(
  Map<String, Object?> overrides,
  Logger logger,
) async {
  final resolved = _resolveInheritedEnvironment(overrides);

  if (!Platform.isWindows) {
    return resolved;
  }
  if (_msvcToolchainIsAvailable(resolved)) {
    return resolved;
  }
  if (overrides['msvc_devshell'] != true) {
    return resolved;
  }

  final developerShell = await _devShellEnvironment(logger);
  return {...resolved, ...developerShell};
}

/// Finds values for [_propagatedVariables] that the hook environment lacks.
///
/// Resolution order per variable, first hit wins:
///
/// 1. the inherited environment (already present, so nothing to do);
/// 2. an explicit `hooks: user_defines: anycodec:` override, keyed by the
///    lower-cased variable name (`cargo_home`, `include`, `lib`, ...);
/// 3. the value registered for the current user's Windows profile
///    (`HKCU\Environment`), which is where a developer's own setup lives.
Map<String, String> _resolveInheritedEnvironment(
  Map<String, Object?> overrides,
) {
  final resolved = <String, String>{};
  for (final name in _propagatedVariables) {
    if ((Platform.environment[name] ?? '').isNotEmpty) {
      continue;
    }
    final override = overrides[name.toLowerCase()];
    var value = override is String && override.isNotEmpty ? override : null;
    value ??= _registeredUserEnvironmentVariable(name);
    if (value == null || value.isEmpty) {
      continue;
    }
    final expanded = _expandEnvironmentVariables(value);
    if (!_isUsableValue(name, expanded)) {
      continue;
    }
    resolved[name] = expanded;
  }
  return resolved;
}

/// `true` when `value` points at something real.
///
/// `INCLUDE`/`LIB`/`LIBPATH` are `;`-separated lists; one surviving entry is
/// enough to be useful, so a partially-broken list is propagated rather than
/// silently dropped.
bool _isUsableValue(String name, String value) {
  final entries = value
      .split(';')
      .where((entry) => entry.trim().isNotEmpty)
      .toList();
  if (entries.isEmpty) {
    return false;
  }
  if (name == 'INCLUDE' || name == 'LIB' || name == 'LIBPATH') {
    return entries.any((entry) => Directory(entry.trim()).existsSync());
  }
  if (name == 'WindowsSDKVersion') {
    // A bare version string such as `10.0.26100.0\`, not a path.
    return true;
  }
  return Directory(value).existsSync() || File(value).existsSync();
}

/// Runs this hook again with `environment` added, inheriting stdio.
///
/// Returns the child's exit code, or `null` when the hook could not be
/// re-executed, in which case the caller carries on with what it has.
Future<int?> _reexecWithEnvironment(
  List<String> args,
  Map<String, String> environment,
) async {
  final script = Platform.script.toFilePath();
  if (script.isEmpty) {
    return null;
  }
  try {
    final process = await Process.start(
      Platform.resolvedExecutable,
      [...Platform.executableArguments, script, ...args],
      environment: {...environment, _reexecMarker: '1'},
      mode: ProcessStartMode.inheritStdio,
    );
    return await process.exitCode;
  } on ProcessException catch (error) {
    stderr.writeln(
      '[anycodec] could not re-execute the build hook with '
      '${environment.keys.join(', ')}: $error',
    );
    return null;
  }
}

/// Reads a variable from `HKCU\Environment` (Windows only).
///
/// Values there may be `REG_EXPAND_SZ`, hence the expansion in the caller.
String? _registeredUserEnvironmentVariable(String name) {
  if (!Platform.isWindows) {
    return null;
  }
  try {
    final result = Process.runSync('reg', [
      'query',
      r'HKCU\Environment',
      '/v',
      name,
    ]);
    if (result.exitCode != 0) {
      return null;
    }
    for (final line in result.stdout.toString().split('\n')) {
      final trimmed = line.trim();
      if (!trimmed.startsWith(name)) {
        continue;
      }
      final parts = trimmed.split(RegExp(r'\s{2,}'));
      if (parts.length < 3) {
        continue;
      }
      final value = parts.sublist(2).join(' ').trim();
      return value.isEmpty ? null : value;
    }
  } on ProcessException {
    return null;
  }
  return null;
}

/// Expands `%NAME%` and `$NAME` / `${NAME}` references from the environment.
String _expandEnvironmentVariables(String value) {
  var expanded = value.replaceAllMapped(
    RegExp(r'%([^%]+)%'),
    (match) => Platform.environment[match.group(1)!] ?? match.group(0)!,
  );
  expanded = expanded.replaceAllMapped(
    RegExp(r'\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?'),
    (match) => Platform.environment[match.group(1)!] ?? match.group(0)!,
  );
  return expanded;
}

// ---------------------------------------------------------------------------
// Visual Studio developer environment
// ---------------------------------------------------------------------------

/// Harvests the MSVC environment from `Enter-VsDevShell`.
///
/// This is Microsoft's documented entry point (the module ships with every
/// Visual Studio that has the C++ workload) and it is the successor to calling
/// `vcvars64.bat` by hand: it also sets `VCToolsInstallDir`, `WindowsSdkDir` and
/// the rest, and it picks the right host/target architecture itself.
///
/// Returns an empty map on any failure; the caller only logs and carries on.
Future<Map<String, String>> _devShellEnvironment(Logger logger) async {
  final installation = _visualStudioInstallation();
  if (installation == null) {
    return const {};
  }
  final module =
      '$installation\\Common7\\Tools\\Microsoft.VisualStudio.DevShell.dll';
  if (!File(module).existsSync()) {
    logger.fine('no DevShell module at $module');
    return const {};
  }
  final instanceId = _vswhere([
    '-latest',
    '-products',
    '*',
    '-property',
    'instanceId',
  ]);
  if (instanceId == null) {
    logger.fine('vswhere did not report an instance id');
    return const {};
  }

  // `Get-ChildItem Env:` gives every variable the dev shell set, as NAME=VALUE.
  final command = StringBuffer()
    ..writeln("Import-Module '$module'")
    ..writeln(
      "Enter-VsDevShell $instanceId -SkipAutomaticLocation "
      "-DevCmdArguments '-arch=x64 -host_arch=x64'",
    )
    ..writeln(
      r'Get-ChildItem Env: | ForEach-Object { "$($_.Name)=$($_.Value)" }',
    );

  try {
    final result = await Process.run('powershell.exe', [
      '-NoProfile',
      '-NonInteractive',
      '-ExecutionPolicy',
      'Bypass',
      '-Command',
      command.toString(),
    ]);
    if (result.exitCode != 0) {
      logger.fine('Enter-VsDevShell exited with ${result.exitCode}');
      return const {};
    }
    final environment = <String, String>{};
    for (final line in (result.stdout as String).split(RegExp(r'\r?\n'))) {
      final separator = line.indexOf('=');
      if (separator <= 0) {
        continue;
      }
      final key = line.substring(0, separator).trim();
      if (key == 'PATH' || _propagatedVariables.contains(key)) {
        environment[key] = line.substring(separator + 1);
      }
    }
    if (environment.isEmpty) {
      logger.fine('Enter-VsDevShell produced no usable variables');
      return const {};
    }
    logger.info(
      'Enter-VsDevShell provided ${environment.keys.join(', ')} '
      'from $installation',
    );
    return environment;
  } on ProcessException catch (error) {
    logger.fine('could not run Enter-VsDevShell: $error');
    return const {};
  }
}

/// The installation path of the newest Visual Studio, or `null`.
///
/// Deliberately does **not** pass `-requires`: the component identifiers differ
/// between Visual Studio generations, and a filter that does not match turns a
/// perfectly good installation into "not found".
String? _visualStudioInstallation() =>
    _vswhere(['-latest', '-products', '*', '-property', 'installationPath']);

/// Runs `vswhere` and returns its first non-empty output line.
///
/// `vswhere.exe` is not on `PATH`, and `ProgramFiles(x86)` is **not** forwarded
/// to a build hook (`hooks_runner` whitelists only `SYSTEMDRIVE`, `PROGRAMDATA`,
/// `WINDIR`, `PATH`, ...). So the well-known absolute location is derived from
/// `SYSTEMDRIVE`, which *is* forwarded.
String? _vswhere(List<String> arguments) {
  for (final vswhere in _vswhereCandidates()) {
    if (!File(vswhere).existsSync()) {
      continue;
    }
    try {
      final result = Process.runSync(vswhere, arguments);
      if (result.exitCode != 0) {
        continue;
      }
      for (final line in (result.stdout as String).split(RegExp(r'\r?\n'))) {
        final trimmed = line.trim();
        if (trimmed.isNotEmpty) {
          return trimmed;
        }
      }
    } on ProcessException {
      continue;
    }
  }
  return null;
}

/// Every place a `vswhere.exe` may live, most likely first.
List<String> _vswhereCandidates() {
  final candidates = <String>[];

  // `ProgramFiles`-style variables only survive when the host set them
  // explicitly; treat them as a bonus, never as the primary route.
  for (final name in ['ProgramFiles(x86)', 'ProgramFiles']) {
    final root = Platform.environment[name];
    if (root != null && root.isNotEmpty) {
      candidates.add('$root\\Microsoft Visual Studio\\Installer\\vswhere.exe');
    }
  }

  final systemDrive = Platform.environment['SYSTEMDRIVE'] ?? 'C:';
  candidates
    ..add(
      '$systemDrive\\Program Files (x86)\\Microsoft Visual Studio\\Installer\\vswhere.exe',
    )
    ..add(
      '$systemDrive\\Program Files\\Microsoft Visual Studio\\Installer\\vswhere.exe',
    );

  // Anything on PATH (some CI images put it there).
  for (final entry in (Platform.environment['PATH'] ?? '').split(';')) {
    if (entry.isNotEmpty) {
      candidates.add('$entry\\vswhere.exe');
    }
  }

  final programData = Platform.environment['PROGRAMDATA'];
  if (programData != null && programData.isNotEmpty) {
    candidates.add(
      '$programData\\Microsoft\\VisualStudio\\Installer\\vswhere.exe',
    );
  }

  return candidates;
}

/// `true` when `rustc` will be able to link for `x86_64-pc-windows-msvc`.
///
/// Mirrors rustc's own discovery closely enough to avoid false alarms: an
/// exported MSVC environment, a `link.exe` on `PATH`, or a Visual Studio
/// installation. `rustc` finds Visual Studio through the registry itself, so the
/// mere existence of an installation is enough — which is why the warning below
/// only fires when there is genuinely nothing on the machine.
bool _msvcToolchainIsAvailable(Map<String, String> extra) {
  String? lookup(String name) {
    final value = extra[name];
    if (value != null && value.isNotEmpty) {
      return value;
    }
    final inherited = Platform.environment[name];
    return (inherited ?? '').isEmpty ? null : inherited;
  }

  if (lookup('VCINSTALLDIR') != null || lookup('VSINSTALLDIR') != null) {
    return true;
  }
  final path = lookup('PATH') ?? '';
  for (final entry in path.split(';')) {
    if (entry.isEmpty) {
      continue;
    }
    if (File('$entry\\link.exe').existsSync()) {
      return true;
    }
  }
  return _visualStudioInstallation() != null;
}

/// Explains a genuinely missing MSVC toolchain, in words `cargo` will not use.
void _warnAboutMissingMsvcToolchain(Logger logger) {
  logger.warning(
    'No Visual Studio C++ toolchain was found. On Windows, `rustc` needs the '
    'MSVC linker: install the "Desktop development with C++" workload, or run '
    'Flutter from a "Developer Command Prompt for VS". If Visual Studio lives '
    'somewhere rustc does not look, ask the hook to obtain the environment from '
    'the developer shell instead:\n'
    '  hooks:\n'
    '    user_defines:\n'
    '      anycodec:\n'
    '        msvc_devshell: true',
  );
}
// ---------------------------------------------------------------------------
// Private temp directory
// ---------------------------------------------------------------------------

/// A `TMP`/`TEMP` pair owned by this hook, inside the build output directory.
///
/// The MSVC librarian and linker build scratch files in `%TEMP%`; a sandbox that
/// blocks writes there turns a working build into
/// `LNK1104: cannot open file '...\lnk{....tmp'`. Pointing `TMP`/`TEMP` at a
/// directory this hook owns makes the build independent of the host
/// configuration and keeps intermediate files inside the build output.
Map<String, String> _privateTempEnvironment(BuildInput input, Logger logger) {
  try {
    final base = input.outputDirectory.toFilePath();
    final separator = base.endsWith('/') || base.endsWith(r'\')
        ? ''
        : Platform.pathSeparator;
    final directory = Directory('$base${separator}tmp')
      ..createSync(recursive: true);
    logger.fine('using ${directory.path} as TMP/TEMP for the Rust build');
    return {'TMP': directory.path, 'TEMP': directory.path};
  } on FileSystemException catch (error) {
    logger.warning(
      'could not create a private temp directory ($error); '
      'falling back to the inherited TMP/TEMP',
    );
    return const {};
  }
}
