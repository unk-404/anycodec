// Verifies what `dart pub publish` would actually ship — before it ships.
//
// Run from the package root:  dart run .github/scripts/check_archive.dart
//
// ## Why this exists
//
// `dart pub publish --dry-run` succeeds whenever the package is *well formed*
// and says nothing about whether it is *complete*. A `.pubignore` pattern that
// is not rooted with `/` matches at any depth, so `build/` silently deletes
// `lib/src/build/` and a whole subtarget can vanish from the archive while every
// remaining file still imports it. The result publishes cleanly, scores
// acceptably, and does not compile for a single consumer.
//
// The checks below are ordered by how early they would have caught that:
//
//   1. Every relative `import`/`export` in a published Dart file resolves to a
//      published file. This is the invariant, not a proxy for it.
//   2. Every source file that exists on disk under `lib/`, `test/` and
//      `rust/src/` also appears in the archive. This catches a bad pattern
//      before it reaches the import graph.
//   3. A required-file list, so a directory cannot disappear wholesale.
//   4. A forbidden-path list, so `.agents/`, `target/` and build output cannot
//      creep back in.
//
// Exit codes: 0 clean, 1 a check failed, 2 the dry run never produced a tree.
//
// ignore_for_file: avoid_print

import 'dart:convert';
import 'dart:io';

/// Files that must be in the archive for the package to be usable at all.
const List<String> _required = [
  'pubspec.yaml',
  'README.md',
  'CHANGELOG.md',
  'LICENSE',
  // NOTE: .pubignore is deliberately absent - dart pub publish treats it as a
  // control file and never ships it, which this check discovered by failing.
  'ffigen.yaml',
  'analysis_options.yaml',
  // The build hook, and the asset id the generated bindings resolve against.
  'hook/build.dart',
  'lib/anycodec.dart',
  'lib/anycodec_platform.dart',
  'lib/src/bindings/anycodec_bindings_generated.dart',
  // Everything the hook compiles.
  'rust/Cargo.toml',
  'rust/Cargo.lock',
  'rust/rust-toolchain.toml',
  'rust/cbindgen.toml',
  'rust/include/anycodec.h',
  // Tests are part of the published package by convention.
  'test/abi_test.dart',
];

/// Prefixes that must never appear in the archive.
const List<String> _forbidden = [
  '.agents/', // the agent's workspace: 5 GB of crate copies
  '.dart_tool/',
  '.git/',
  '.vscode/',
  'build/',
  'coverage/',
  'doc/api/',
  'rust/target/', // ~3 GB of build output
  'pubspec.lock', // libraries must not pin their consumers
  'example/pubspec.lock',
  'example/build/',
  'example/.dart_tool/',
  'example/linux/flutter/ephemeral/',
  'example/macos/Flutter/ephemeral/',
  'example/windows/flutter/ephemeral/',
  'example/ios/Flutter/ephemeral/',
];

/// Directories whose on-disk contents must all survive into the archive.
const List<String> _mustBeComplete = ['lib', 'test', 'rust/src'];

/// A compressed archive larger than this means something huge got included.
const int _sizeCeilingKb = 20 * 1024;

void main(List<String> arguments) {
  // The package under test is located from this script's own path, not from the
  // current directory. In a monorepo the caller may be anywhere — CI runs it from
  // the repository root, a developer may run it from `packages/anycodec` — and a
  // check that silently measures the wrong directory is worse than no check.
  final root = arguments.isEmpty
      ? Directory(_join(_repoRoot(), 'packages/anycodec'))
      : Directory(arguments.first);
  if (!File(_join(root.path, 'pubspec.yaml')).existsSync()) {
    stderr.writeln(
      'check_archive: ${root.path} is not a package (no pubspec.yaml).\n'
      'Pass the package directory explicitly: '
      'dart run .github/scripts/check_archive.dart <package>',
    );
    exit(2);
  }

  final output = _runDryRun(root);
  if (output == null) {
    exit(2);
  }

  final archive = _parseTree(output);
  if (archive == null) {
    stderr.writeln(
      'check_archive: could not find the archive tree in the dry-run output.\n'
      'The output format changed; the parser below needs updating. Raw output:\n'
      '$output',
    );
    exit(2);
  }

  final failures = <String>[];

  // (4) forbidden paths. Run first: these are the expensive mistakes.
  for (final entry in archive.files) {
    for (final bad in _forbidden) {
      if (entry == bad ||
          entry.startsWith(bad) ||
          entry == bad.substring(0, bad.length - 1)) {
        failures.add('forbidden path in the archive: $entry (matches $bad)');
      }
    }
  }

  // (3) required files.
  for (final needed in _required) {
    if (!archive.files.contains(needed)) {
      failures.add('missing from the archive: $needed');
    }
  }

  // (2) nothing on disk was silently dropped.
  for (final dir in _mustBeComplete) {
    for (final file in _walk(Directory(_join(root.path, dir)))) {
      final relative = _relative(root.path, file.path);
      if (!archive.files.contains(relative)) {
        failures.add('$relative exists on disk but is not in the archive');
      }
    }
  }

  // (1) every published Dart file's relative imports resolve inside the archive.
  var importsChecked = 0;
  for (final entry in archive.files) {
    if (!entry.endsWith('.dart') || !entry.startsWith('lib/')) {
      continue;
    }
    final file = File(_join(root.path, entry));
    if (!file.existsSync()) {
      // A generated file that is not on disk is acceptable; it is the archive
      // that matters, and the import check below only needs the sources.
      continue;
    }
    final source = file.readAsStringSync();
    for (final uri in _relativeUris(source)) {
      importsChecked++;
      final resolved = _resolve(entry, uri);
      if (resolved == null) {
        failures.add('$entry: cannot resolve relative URI "$uri"');
        continue;
      }
      if (!archive.files.contains(resolved)) {
        failures.add(
          '$entry imports "$uri" -> $resolved, which is NOT in the archive',
        );
      }
    }
  }

  // The dry run reports errors and warnings in the same block; only errors are
  // fatal. The exit code is useless here — it is 65 for a warning too, which is
  // why this script does not consult it.
  if (output.contains('Package validation found the following error')) {
    failures.add('dart pub publish reported a validation error (see above)');
  }

  final sizeKb = archive.sizeKb;
  if (sizeKb != null && sizeKb > _sizeCeilingKb) {
    failures.add(
      'archive is ${sizeKb ~/ 1024} MB, over the ${_sizeCeilingKb ~/ 1024} MB '
      'ceiling — something huge was included',
    );
  }

  print(
    'archive: ${archive.files.length} files'
    '${sizeKb == null ? '' : ', ${(sizeKb / 1024).toStringAsFixed(1)} MB compressed'}',
  );
  print('relative imports checked: $importsChecked');

  if (failures.isEmpty) {
    print('check_archive: OK');
    return;
  }
  stderr.writeln('\ncheck_archive: ${failures.length} problem(s)');
  for (final failure in failures) {
    stderr.writeln('  - $failure');
  }
  exit(1);
}

// ---------------------------------------------------------------------------
// Running and parsing
// ---------------------------------------------------------------------------

String? _runDryRun(Directory root) {
  // `dart` is `dart.bat` on Windows, which only resolves through a shell.
  //
  // `stdoutEncoding: utf8` is mandatory, not tidiness. Without it Dart decodes
  // the child's output with the *system code page* (CP936 on this machine), which
  // mangles the tree's box-drawing characters into unrelated code points, so the
  // parser silently matches nothing and this script reports "no archive tree".
  // It is the same class of bug as `Set-Content -Encoding utf8` corrupting a
  // UTF-8 source file: an encoding default, chosen by the tool, quietly deciding
  // what the data means.
  final result = Process.runSync(
    'dart',
    ['pub', 'publish', '--dry-run'],
    workingDirectory: root.path,
    runInShell: Platform.isWindows,
    stdoutEncoding: utf8,
    stderrEncoding: utf8,
  );
  final output = '${result.stdout}\n${result.stderr}';
  if (!output.contains('Total compressed archive size')) {
    stderr.writeln('check_archive: the dry run did not produce an archive');
    stderr.writeln(output);
    return null;
  }
  return output;
}

class _Archive {
  _Archive(this.files, this.sizeKb);
  final Set<String> files;
  final int? sizeKb;
}

/// Turns the dry run's tree rendering into a set of paths.
///
/// The format is `├── name` / `└── name`, indented by four characters per level
/// with `│   ` for continuing branches. Directories carry no size suffix; files
/// carry ` (N KB)` or ` (<1 KB)`.
_Archive? _parseTree(String output) {
  final files = <String>{};
  final stack = <String>[];
  var sawTree = false;
  int? sizeKb;

  final sizeMatch = RegExp(r'Total compressed archive size: (\d+) KB')
      .firstMatch(output);
  if (sizeMatch != null) {
    sizeKb = int.parse(sizeMatch.group(1)!);
  }

  for (final line in output.split('\n')) {
    final marker = line.indexOf('├── ');
    final tail = line.indexOf('└── ');
    final cut = marker >= 0 ? marker : tail;
    if (cut < 0) {
      continue;
    }
    sawTree = true;
    final depth = cut ~/ 4;
    var rest = line.substring(cut + 4).trimRight();
    // Strip the trailing ` (N KB)` / ` (<1 KB)`.
    final isFile = RegExp(r' \((<\d+|\d+) [KM]?B\)$').hasMatch(rest);
    if (isFile) {
      rest = rest.substring(0, rest.lastIndexOf(' ('));
    }
    if (depth < stack.length) {
      stack.removeRange(depth, stack.length);
    }
    stack.add(rest);
    if (isFile) {
      files.add(stack.join('/'));
    }
  }

  return sawTree ? _Archive(files, sizeKb) : null;
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

List<File> _walk(Directory directory) {
  if (!directory.existsSync()) {
    return const [];
  }
  return directory
      .listSync(recursive: true, followLinks: false)
      .whereType<File>()
      .toList();
}

/// The relative `import` / `export` / `part` URIs in `source`.
Iterable<String> _relativeUris(String source) sync* {
  final pattern = RegExp(
    r'''^\s*(?:import|export|part)\s+['"]([^'"]+)['"]''',
    multiLine: true,
  );
  for (final match in pattern.allMatches(source)) {
    final uri = match.group(1)!;
    if (uri.startsWith('.') || uri.startsWith('/')) {
      yield uri;
    }
  }
}

/// Resolves `uri` against the directory holding `from`, as a package-relative
/// path. Returns `null` when the URI cannot be normalised.
String? _resolve(String from, String uri) {
  if (uri.startsWith('/')) {
    return _normalise(uri.substring(1));
  }
  final segments = from.split('/')..removeLast();
  for (final part in uri.split('/')) {
    if (part == '.' || part.isEmpty) {
      continue;
    }
    if (part == '..') {
      if (segments.isEmpty) {
        return null;
      }
      segments.removeLast();
      continue;
    }
    segments.add(part);
  }
  return segments.join('/');
}

String? _normalise(String path) {
  final segments = <String>[];
  for (final part in path.split('/')) {
    if (part.isEmpty || part == '.') {
      continue;
    }
    if (part == '..') {
      if (segments.isEmpty) {
        return null;
      }
      segments.removeLast();
      continue;
    }
    segments.add(part);
  }
  return segments.join('/');
}

String _relative(String root, String path) {
  final normalised = path.replaceAll(r'\', '/');
  final prefix = root.replaceAll(r'\', '/');
  return normalised.startsWith('$prefix/')
      ? normalised.substring(prefix.length + 1)
      : normalised;
}

String _join(String a, String b) => a.endsWith(Platform.pathSeparator)
    ? '$a$b'
    : '$a${Platform.pathSeparator}$b';

/// The repository root, derived from this script's own location.
///
/// The script lives at `<root>/.github/scripts/check_archive.dart`, so three
/// levels up is the root regardless of the caller's working directory. Deriving
/// it beats relying on the cwd: in a monorepo CI runs from the root and a
/// developer from `packages/anycodec`, and a check that silently measures the
/// wrong directory is worse than no check at all.
String _repoRoot() {
  var directory = File.fromUri(Platform.script).absolute.parent; // .github/scripts
  directory = directory.parent; // .github
  return directory.parent.path; // <root>
}
