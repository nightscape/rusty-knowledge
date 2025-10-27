import 'package:flutter_test/flutter_test.dart';
import 'package:rusty_knowledge/src/rust/api/ui_test_bridge.dart';
import 'package:rusty_knowledge/src/rust/frb_generated.dart';

/// Minimal proof-of-concept test demonstrating Rust PBT → Flutter UI communication
///
/// This test validates that we can:
/// 1. Start a Rust-based test from Flutter
/// 2. Receive UI commands from Rust
/// 3. Execute commands (mocked here, would be real UI in integration test)
/// 4. Send results back to Rust
/// 5. Get validation result from Rust
void main() {
  setUpAll(() async {
    // Initialize flutter_rust_bridge
    await RustLib.init();
  });

  group('UI Test Bridge - Bidirectional Communication', () {
    test('Basic round-trip: Flutter → Rust → Flutter → Rust', () async {
      // Step 1: Flutter starts a Rust test
      final testId = await startUiTest();
      expect(testId, isNotEmpty);
      print('✓ Started UI test with ID: $testId');

      // Step 2: Get commands from Rust
      final commands = await getNextCommands(testId: testId);
      expect(commands, isNotEmpty);
      print('✓ Received ${commands.length} commands from Rust:');
      for (var cmd in commands) {
        print('  - ${cmd.toString()}');
      }

      // Step 3: Execute commands (mocked for unit test)
      // In a real integration test, this would drive actual UI widgets
      final results = <UiCommandResult>[];

      for (var command in commands) {
        // Pattern match on the command type and execute
        command.when(
          createBlock: (parentId, content, commandId) {
            print(
              '  Executing: CreateBlock(parent=$parentId, content="$content")',
            );
            // Mock: simulate successful block creation
            results.add(
              UiCommandResult.blockCreated(
                commandId: commandId,
                blockId: 'mock-block-123',
              ),
            );
          },
          updateBlock: (id, content, commandId) {
            print('  Executing: UpdateBlock(id=$id, content="$content")');
            results.add(UiCommandResult.blockUpdated(commandId: commandId));
          },
          deleteBlock: (id, commandId) {
            print('  Executing: DeleteBlock(id=$id)');
            results.add(UiCommandResult.blockDeleted(commandId: commandId));
          },
          verifyBlockExists: (id, expectedContent, commandId) {
            print(
              '  Executing: VerifyBlockExists(id=$id, expected="$expectedContent")',
            );
            // Mock: simulate successful verification
            results.add(
              UiCommandResult.verificationPassed(commandId: commandId),
            );
          },
        );
      }
      print('✓ Executed ${results.length} commands in Flutter (mocked)');

      // Step 4: Submit results back to Rust
      final validationResult = await submitResults(
        testId: testId,
        results: results,
      );

      // Step 5: Check if test completed and passed
      if (validationResult != null) {
        validationResult.when(
          passed: (message) {
            print('✓ Rust validation PASSED: $message');
            expect(message, contains('passed'));
          },
          failed: (error) {
            fail('Rust validation FAILED: $error');
          },
        );
      } else {
        print('⚠ Test not yet complete (more commands pending)');
      }

      // Cleanup
      await cleanupTest(testId: testId);
      print('✓ Test cleanup complete');
    });

    test('Error handling: Rust receives error from Flutter', () async {
      final testId = await startUiTest();
      final commands = await getNextCommands(testId: testId);

      // Simulate an error during command execution
      final results = [
        UiCommandResult.error(
          commandId: 1,
          message: 'Simulated UI error: widget not found',
        ),
      ];

      final validationResult = await submitResults(
        testId: testId,
        results: results,
      );

      // Rust should report the test as failed
      validationResult?.when(
        passed: (_) => fail('Expected validation to fail'),
        failed: (error) {
          expect(error, contains('Command 1 failed'));
          expect(error, contains('Simulated UI error'));
          print('✓ Error propagated correctly from Flutter to Rust');
        },
      );

      await cleanupTest(testId: testId);
    });
  });
}
