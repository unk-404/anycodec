// Structurally validates every workflow in `.github/workflows/`.
//
// Run from the package root:  dart run .github/scripts/validate_workflows.dart
//
// ## Why
//
// A GitHub Actions workflow is not executed by anything on a developer machine,
// and a mistake in it is invisible until it runs in CI — where it is expensive,
// asynchronous, and shows up as a red X on someone else's pull request. The usual
// failure modes are not syntax errors; they are YAML that parses and means the
// wrong thing:
//
//   * a step silently missing its `shell:`
//   * a job with no `timeout-minutes` that occupies a runner for six hours
//   * a `runs-on` typo, or a job quietly losing the one command it existed to run
//   * a publish job without `id-token: write`, which fails on pub.dev with an
//     opaque 403 rather than anything resembling a permissions error
//
// This parses the files the way Actions does and asserts the properties that are
// easy to lose by editing. It is not a substitute for the workflow running; it is
// the check that makes editing it safe.
//
// Exit codes: 0 clean, 1 a check failed, 2 a file could not be read/parsed.
//
// ignore_for_file: avoid_print

import 'dart:io';

import 'package:yaml/yaml.dart';

/// Commands each job must contain, so a job cannot lose its reason to exist.
const Map<String, List<String>> _requiredPerJob = {
  'dart-quality': [
    'flutter pub get',
    'dart analyze',
    'dart analyze hook',
    'dart analyze .github',
    'flutter analyze',
    'check_archive.dart',
    'validate_workflows.dart',
    'unresolved doc reference',
    'libgstreamer1.0-dev',
  ],
  'rust-tests': [
    'ubuntu-latest',
    'windows-latest',
    'macos-14',
    'cargo clippy',
    'cargo test',
    'libgstreamer1.0-dev',
    '-- --ignored',
  ],
  'dart-tests': ['ubuntu-latest', 'windows-latest', 'macos-14', 'dart test'],
  'cross-check': [
    'aarch64-linux-android',
    'armv7-linux-androideabi',
    'x86_64-linux-android',
    'aarch64-apple-darwin',
    'x86_64-apple-darwin',
    'aarch64-apple-ios',
    'aarch64-apple-ios-sim',
    'x86_64-apple-ios',
    'cargo check',
  ],
  'dependency-floor': [
    'dart pub downgrade --no-example',
    'dart analyze',
    'dart compile kernel hook/build.dart',
  ],
  'publish': [
    'check_archive.dart',
    'validate_workflows.dart',
    'unresolved doc reference',
    'libgstreamer1.0-dev',
    'dart pub publish --dry-run',
    'dart pub publish --force',
    'Tag matches pubspec',
  ],
};

void main() {
  final directory = Directory('.github/workflows');
  if (!directory.existsSync()) {
    stderr.writeln('validate_workflows: .github/workflows/ not found');
    exit(2);
  }
  final files =
      directory
          .listSync()
          .whereType<File>()
          .where(
            (file) => file.path.endsWith('.yml') || file.path.endsWith('.yaml'),
          )
          .toList()
        ..sort((a, b) => a.path.compareTo(b.path));

  if (files.isEmpty) {
    stderr.writeln('validate_workflows: no workflow files found');
    exit(2);
  }

  final failures = <String>[];
  var jobCount = 0;
  var stepCount = 0;

  for (final file in files) {
    final name = file.uri.pathSegments.last;
    final Object? document;
    try {
      document = loadYaml(file.readAsStringSync());
    } on YamlException catch (error) {
      stderr.writeln('validate_workflows: $name is not valid YAML: $error');
      exit(2);
    }
    if (document is! YamlMap) {
      failures.add('$name: the workflow is not a mapping');
      continue;
    }

    // `on` is the trigger key. YAML 1.1 readers turn a bare `on` into the boolean
    // true, so both spellings are accepted rather than silently seeing no
    // triggers and reporting nothing.
    final triggers = document['on'] ?? document[true];
    if (triggers == null) {
      failures.add('$name: no `on:` trigger block');
    } else {
      final text = triggers.toString();
      // Nothing in this project should run on a timer: a scheduled job that
      // starts failing is noticed late, and these builds are not time-sensitive.
      if (text.contains('schedule')) {
        failures.add(
          '$name: has a `schedule` trigger; CI here is on-demand only',
        );
      }
      if (!text.contains('pull_request')) {
        failures.add(
          '$name: no `pull_request` trigger, so CI would not run on a PR',
        );
      }
    }

    final jobs = document['jobs'];
    if (jobs is! YamlMap || jobs.isEmpty) {
      failures.add('$name: no `jobs:` mapping');
      continue;
    }

    for (final node in jobs.entries) {
      jobCount++;
      final jobName = '${node.key}';
      final label = '$name job `$jobName`';
      final job = node.value;
      if (job is! YamlMap) {
        failures.add('$label is not a mapping');
        continue;
      }
      if (!job.containsKey('runs-on')) {
        failures.add('$label has no `runs-on`');
      }
      // Without this a wedged job holds a runner for the six-hour default.
      if (!job.containsKey('timeout-minutes')) {
        failures.add('$label has no `timeout-minutes`');
      }

      // Automated publishing is an OIDC exchange. Missing `id-token: write`
      // surfaces as a bare 403 from pub.dev, which reads like an auth problem
      // rather than a permissions one.
      if (jobName == 'publish') {
        final permissions = job['permissions'];
        final text = permissions == null ? '' : _flatten(permissions);
        if (!text.contains('id-token: write')) {
          failures.add(
            '$label needs `permissions: id-token: write` for OIDC publishing',
          );
        }
      }

      final steps = job['steps'];
      if (steps is! YamlList || steps.isEmpty) {
        failures.add('$label has no steps');
        continue;
      }

      var stepIndex = 0;
      for (final step in steps) {
        stepIndex++;
        stepCount++;
        if (step is! YamlMap) {
          failures.add('$label step $stepIndex is not a mapping');
          continue;
        }
        final stepLabel = '$label step $stepIndex';
        // Every command must run under bash: the default shell on a Windows
        // runner turns a native program's stderr into a terminating error, and
        // PowerShell's file APIs default to the system code page, which silently
        // corrupts UTF-8.
        if (step.containsKey('run') && !step.containsKey('shell')) {
          failures.add('$stepLabel has a `run:` with no `shell: bash`');
        }
        if (step.containsKey('shell') && '${step['shell']}' != 'bash') {
          failures.add(
            '$stepLabel uses shell `${step['shell']}` instead of bash',
          );
        }
        // A step with neither `uses` nor `run` does nothing at all.
        if (!step.containsKey('run') && !step.containsKey('uses')) {
          failures.add('$stepLabel has neither `run` nor `uses`');
        }
      }

      final body = _flatten(job);
      for (final needed in _requiredPerJob[jobName] ?? const <String>[]) {
        if (!body.contains(needed)) {
          failures.add('$label no longer contains `$needed`');
        }
      }
    }
  }

  print('workflows: ${files.length} files, $jobCount jobs, $stepCount steps');
  for (final file in files) {
    print('  ${file.uri.pathSegments.last}');
  }

  if (failures.isEmpty) {
    print('validate_workflows: OK');
    return;
  }
  stderr.writeln('\nvalidate_workflows: ${failures.length} problem(s)');
  for (final failure in failures) {
    stderr.writeln('  - $failure');
  }
  exit(1);
}

/// Every scalar in a subtree, joined — enough to ask "does this job mention X".
String _flatten(Object? node) {
  final buffer = StringBuffer();
  void walk(Object? value) {
    if (value is YamlMap) {
      for (final entry in value.entries) {
        // key: value, not key value: several checks below look for a YAML
        // mapping entry such as id-token: write, which a space would never match.
        buffer.write('${entry.key}: ');
        walk(entry.value);
      }
    } else if (value is YamlList) {
      for (final item in value) {
        walk(item);
      }
    } else if (value != null) {
      buffer.write('$value ');
    }
  }

  walk(node);
  return buffer.toString();
}
