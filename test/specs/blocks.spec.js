import { expect } from '@wdio/globals';

describe('Block Editor', () => {
  it('should allow adding a block', async () => {
    // Wait for app to load
    await browser.pause(2000);

    // Find and click the "Add Block" button
    const addButton = await $('button*=Add Block');
    await addButton.waitForDisplayed({ timeout: 5000 });
    await addButton.click();

    // Wait a bit for the block to be added
    await browser.pause(1000);

    // Check if the editor appeared (TipTap creates a .ProseMirror element)
    const editor = await $('.ProseMirror');
    await editor.waitForDisplayed({ timeout: 5000 });

    expect(await editor.isDisplayed()).toBe(true);
  });

  it('should allow typing text with spaces in the block editor', async () => {
    // Wait for app to load
    await browser.pause(2000);

    // Find and click the "Add Block" button
    const addButton = await $('button*=Add Block');
    await addButton.waitForDisplayed({ timeout: 5000 });
    await addButton.click();

    // Wait for the editor to appear
    await browser.pause(1000);

    // Find the TipTap editor
    const editor = await $('.ProseMirror');
    await editor.waitForDisplayed({ timeout: 5000 });

    // Click to focus the editor
    await editor.click();

    // Try to type text with spaces
    await browser.keys(['H', 'e', 'l', 'l', 'o', ' ', 'W', 'o', 'r', 'l', 'd']);

    // Wait a moment for the text to be entered
    await browser.pause(500);

    // Get the text content
    const text = await editor.getText();

    console.log('Editor text content:', text);

    // Verify the text includes a space
    expect(text).toContain('Hello World');
  });

  it('should allow typing using setValue', async () => {
    // Wait for app to load
    await browser.pause(2000);

    // Find and click the "Add Block" button
    const addButton = await $('button*=Add Block');
    await addButton.waitForDisplayed({ timeout: 5000 });
    await addButton.click();

    // Wait for the editor to appear
    await browser.pause(1000);

    // Find the TipTap editor
    const editor = await $('.ProseMirror');
    await editor.waitForDisplayed({ timeout: 5000 });

    // Click to focus the editor
    await editor.click();

    // Try using setValue which might work differently
    await editor.setValue('Testing with spaces here');

    // Wait a moment
    await browser.pause(500);

    // Get the text content
    const text = await editor.getText();

    console.log('Editor text content (setValue):', text);

    // Verify the text was entered
    expect(text).toContain('Testing');
  });
});
