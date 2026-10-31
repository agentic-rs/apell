import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import { afterEach, beforeAll, beforeEach, expect, it } from 'vitest';
import type { LlmMessage, LlmResult, LlmStatus } from '../../../packages/applet-sdk/src/types';
import init, * as chatbot from '../pkg/chatbot.js';

let directory: string;
let databasePath: string;
let database: DatabaseSync;
let completionStatus: LlmStatus | 'bridge_failure';
let missingMessage: boolean;
let sentMessages: LlmMessage[];
const assistantMessage: LlmMessage = {
  role: 'assistant',
  content: 'A helpful answer.',
  reasoning_content: 'provider-private replay text',
  reasoning_details: [{ type: 'provider-private', signature: 'opaque-signature', data: null }],
  vendor_metadata: { nested: [true, 3, { preserved: 'exactly' }], empty: '' },
};
const provenance = {
  connection_id: 'test_connection',
  requested_model_id: 'requested_model',
  model_id: 'reported_model',
};

function openDatabase(): void {
  database = new DatabaseSync(databasePath);
  database.exec('PRAGMA foreign_keys = ON');
}

beforeAll(async () => {
  await init({ module_or_path: readFileSync(new URL('../pkg/chatbot_bg.wasm', import.meta.url)) });
});

beforeEach(() => {
  directory = mkdtempSync(join(tmpdir(), 'apell-chatbot-'));
  databasePath = join(directory, 'chatbot.sqlite');
  openDatabase();
  completionStatus = 'completed';
  missingMessage = false;
  sentMessages = [];
});

afterEach(() => {
  database.close();
  rmSync(directory, { recursive: true, force: true });
});

function sqlValue(value: unknown): unknown {
  if (value === null) return { type: 'null' };
  if (typeof value === 'bigint') return { type: 'integer', value: value.toString() };
  if (typeof value === 'number') return { type: 'real', value };
  if (value instanceof Uint8Array) return { type: 'blob', value: Array.from(value) };
  return { type: 'text', value };
}

Object.assign(globalThis, {
  apellCreateId: randomUUID,
  apellHostCall: async (method: string, params: Record<string, unknown>) => {
    if (method === 'sqlite.open') return { database_handle: 'test_database' };
    if (method === 'sqlite.begin') {
      database.exec('BEGIN IMMEDIATE');
      return { transaction_handle: 'test_transaction' };
    }
    if (method === 'sqlite.commit') {
      database.exec('COMMIT');
      return null;
    }
    if (method === 'sqlite.rollback') {
      database.exec('ROLLBACK');
      return null;
    }
    if (method === 'sqlite.query' || method === 'sqlite.execute') {
      const statement = database.prepare(String(params.sql));
      statement.setReadBigInts(true);
      const values = (
        params.parameters as { type: string; value?: string | number | number[] }[]
      ).map((parameter) => {
        if (parameter.type === 'null') return null;
        if (parameter.type === 'integer') return BigInt(String(parameter.value));
        if (parameter.type === 'blob') return Uint8Array.from(parameter.value as number[]);
        return parameter.value as string | number;
      });
      if (method === 'sqlite.query') {
        const rows = statement.all(...values);
        return {
          columns: statement.columns().map((column) => column.name),
          rows: rows.map((row) => Object.values(row).map(sqlValue)),
        };
      }
      const result = statement.run(...values);
      return {
        affected_rows: Number(result.changes),
        last_insert_row_id: result.lastInsertRowid.toString(),
      };
    }
    if (method === 'llm.generate') {
      sentMessages = params.messages as LlmMessage[];
      expect(params).not.toHaveProperty('api_key');
      if (completionStatus === 'bridge_failure')
        throw Object.assign(new Error('Bridge interrupted.'), {
          partial_content: 'Received before bridge failure',
        });
      const content =
        completionStatus === 'completed'
          ? 'A helpful answer.'
          : `Authoritative ${completionStatus} partial`;
      const tool_call = { id: 'call_one', name: 'lookup', arguments: '{}' };
      const result: LlmResult = {
        request_id: String(params.request_id),
        status: completionStatus,
        content,
        tool_calls: completionStatus === 'tool_calls' ? [tool_call] : [],
        message:
          completionStatus === 'completed'
            ? missingMessage
              ? null
              : assistantMessage
            : completionStatus === 'tool_calls'
              ? {
                  role: 'assistant',
                  content,
                  reasoning_content: 'provider-private tool reasoning',
                  tool_calls: [
                    {
                      id: tool_call.id,
                      type: 'function',
                      function: { name: 'lookup', arguments: '{}' },
                    },
                  ],
                }
              : null,
        finish_reason: completionStatus === 'completed' ? 'stop' : null,
        provenance,
        error:
          completionStatus === 'completed' || completionStatus === 'tool_calls'
            ? null
            : { code: completionStatus, message: `Reply ${completionStatus}.`, retryable: false },
      };
      return result;
    }
    throw new Error(`Unexpected host method: ${method}`);
  },
});

it('migrates existing text conversations without losing their messages or replay history', async () => {
  database.exec(`
    CREATE TABLE conversations (conversation_id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
    CREATE TABLE messages (message_id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES conversations(conversation_id) ON DELETE CASCADE, role TEXT NOT NULL CHECK(role IN ('user', 'assistant')), content TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('pending', 'complete', 'interrupted')), request_id TEXT NOT NULL, created_at TEXT NOT NULL);
    CREATE INDEX messages_by_conversation ON messages(conversation_id);
    INSERT INTO conversations VALUES ('legacy', 'Existing conversation', '2026-01-01', '2026-01-01');
    INSERT INTO messages VALUES ('old_user', 'legacy', 'user', 'An existing question', 'complete', 'old_reply', '2026-01-01');
    INSERT INTO messages VALUES ('old_assistant', 'legacy', 'assistant', 'An existing answer', 'complete', 'old_reply', '2026-01-01');
    PRAGMA user_version = 1;
  `);
  await chatbot.initialize();
  expect(database.prepare('PRAGMA user_version').get()?.user_version).toBe(2);
  expect(await chatbot.load_messages('legacy')).toEqual([
    { message_id: 'old_user', role: 'user', content: 'An existing question', status: 'complete' },
    {
      message_id: 'old_assistant',
      role: 'assistant',
      content: 'An existing answer',
      status: 'complete',
    },
  ]);
  await chatbot.send_message('legacy', 'Continue this conversation', 'new_reply');
  expect(sentMessages.slice(1, 3)).toEqual([
    { role: 'user', content: 'An existing question' },
    { role: 'assistant', content: 'An existing answer' },
  ]);
  expect(await chatbot.list_conversations()).toEqual([
    expect.objectContaining({ conversation_id: 'legacy', title: 'Existing conversation' }),
  ]);
});

it('persists complete assistant metadata and attribution and replays it after SQLite reopens', async () => {
  await chatbot.initialize();
  const conversation_id = await chatbot.create_conversation();
  await chatbot.send_message(conversation_id, 'Help me plan\nmy day', 'first_reply');
  const stored = database
    .prepare("SELECT provider_message, provenance FROM messages WHERE role = 'assistant'")
    .get()!;
  expect(JSON.parse(String(stored.provider_message))).toEqual(assistantMessage);
  expect(JSON.parse(String(stored.provenance))).toEqual(provenance);
  const visible = await chatbot.load_messages(conversation_id);
  expect(visible).toEqual([
    expect.objectContaining({ role: 'user', content: 'Help me plan\nmy day', status: 'complete' }),
    expect.objectContaining({
      role: 'assistant',
      content: 'A helpful answer.',
      status: 'complete',
    }),
  ]);
  expect(JSON.stringify(visible)).not.toContain('provider-private');
  expect(JSON.stringify(visible)).not.toContain('opaque-signature');

  database.close();
  openDatabase();
  await chatbot.initialize();
  expect(await chatbot.load_messages(conversation_id)).toEqual(visible);
  await chatbot.send_message(conversation_id, 'Follow up after reopening', 'second_reply');
  expect(sentMessages[2]).toEqual(assistantMessage);
  expect(sentMessages[3]).toEqual({ role: 'user', content: 'Follow up after reopening' });
  expect(await chatbot.list_conversations()).toEqual([
    expect.objectContaining({ conversation_id, title: 'Help me plan my day' }),
  ]);
});

it('retains interrupted partials without replaying them or executing tools', async () => {
  await chatbot.initialize();
  const conversation_id = await chatbot.create_conversation();
  for (const status of ['cancelled', 'truncated', 'unsupported', 'failed', 'tool_calls'] as const) {
    completionStatus = status;
    await expect(
      chatbot.send_message(conversation_id, `Question with ${status} reply`, `${status}_reply`),
    ).rejects.toThrow(
      status === 'tool_calls' ? 'This chatbot requires a text reply.' : `Reply ${status}.`,
    );
    const stored = database
      .prepare(
        "SELECT content, status, provider_message FROM messages WHERE request_id = ? AND role = 'assistant'",
      )
      .get(`${status}_reply`)!;
    expect(stored.content).toBe(`Authoritative ${status} partial`);
    expect(stored.status).toBe('interrupted');
    if (status === 'tool_calls') {
      expect(JSON.parse(String(stored.provider_message)).tool_calls[0]).toEqual({
        id: 'call_one',
        type: 'function',
        function: { name: 'lookup', arguments: '{}' },
      });
    } else {
      expect(stored.provider_message).toBeNull();
    }
    expect(
      sentMessages.some((message) => message.role === 'assistant' || message.role === 'tool'),
    ).toBe(false);
  }
  completionStatus = 'bridge_failure';
  await expect(
    chatbot.send_message(conversation_id, 'Bridge failure question', 'bridge_reply'),
  ).rejects.toThrow('Bridge interrupted.');
  expect(await chatbot.load_messages(conversation_id)).toEqual(
    expect.arrayContaining([
      expect.objectContaining({
        role: 'assistant',
        content: 'Received before bridge failure',
        status: 'interrupted',
      }),
    ]),
  );
  database.prepare("UPDATE messages SET status = 'pending' WHERE role = 'assistant'").run();
  await chatbot.initialize();
  expect(
    database.prepare("SELECT COUNT(*) AS count FROM messages WHERE status = 'pending'").get()
      ?.count,
  ).toBe(0);
});

it('never marks a newly completed answer replayable when its assistant message is missing', async () => {
  await chatbot.initialize();
  missingMessage = true;
  const conversation_id = await chatbot.create_conversation();
  await expect(
    chatbot.send_message(conversation_id, 'Question', 'missing_message'),
  ).rejects.toThrow('The model did not return a replayable assistant message.');
  expect(
    database
      .prepare("SELECT content, status, provider_message FROM messages WHERE role = 'assistant'")
      .get(),
  ).toEqual({
    content: 'A helpful answer.',
    status: 'interrupted',
    provider_message: null,
  });
});
