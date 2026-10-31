import { readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import { afterAll, expect, it } from 'vitest';
import type { LlmResult, LlmStatus } from '../../../packages/applet-sdk/src/types';
import init, * as chatbot from '../pkg/chatbot.js';

const database = new DatabaseSync(':memory:');
database.exec('PRAGMA foreign_keys = ON');
let completionStatus: LlmStatus | 'bridge_failure' = 'completed';
let sentMessages: { role: string; content: string }[] = [];

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
      sentMessages = params.messages as typeof sentMessages;
      expect(params).not.toHaveProperty('api_key');
      if (completionStatus === 'bridge_failure')
        throw Object.assign(new Error('Bridge interrupted.'), {
          partial_content: 'Received before bridge failure',
        });
      const result: LlmResult = {
        request_id: String(params.request_id),
        status: completionStatus,
        content:
          completionStatus === 'completed'
            ? 'A helpful answer.'
            : `Authoritative ${completionStatus} partial`,
        tool_calls: [],
        continuation_id: null,
        finish_reason: completionStatus === 'completed' ? 'stop' : null,
        provenance: {
          connection_id: 'test_connection',
          requested_model_id: 'requested_model',
          model_id: 'reported_model',
        },
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

afterAll(() => {
  database.close();
});

it('runs WASM schema, history, completion and interruption against host SQLite', async () => {
  await init({ module_or_path: readFileSync(new URL('../pkg/chatbot_bg.wasm', import.meta.url)) });
  await chatbot.initialize();
  const conversationId = await chatbot.create_conversation();
  await chatbot.send_message(conversationId, 'Help me plan\nmy day', 'first_reply');
  expect(await chatbot.list_conversations()).toEqual([
    expect.objectContaining({ conversation_id: conversationId, title: 'Help me plan my day' }),
  ]);
  expect(await chatbot.load_messages(conversationId)).toEqual([
    expect.objectContaining({ role: 'user', content: 'Help me plan\nmy day', status: 'complete' }),
    expect.objectContaining({
      role: 'assistant',
      content: 'A helpful answer.',
      status: 'complete',
    }),
  ]);
  for (const status of ['cancelled', 'truncated', 'unsupported', 'failed', 'tool_calls'] as const) {
    completionStatus = status;
    await expect(
      chatbot.send_message(conversationId, `Question with ${status} reply`, `${status}_reply`),
    ).rejects.toThrow(
      status === 'tool_calls' ? 'This chatbot requires a text reply.' : `Reply ${status}.`,
    );
    expect(await chatbot.load_messages(conversationId)).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          role: 'assistant',
          content: `Authoritative ${status} partial`,
          status: 'interrupted',
        }),
      ]),
    );
    expect(sentMessages.some((message) => message.content.startsWith('Authoritative'))).toBe(false);
  }
  completionStatus = 'bridge_failure';
  await expect(
    chatbot.send_message(conversationId, 'Bridge failure question', 'bridge_reply'),
  ).rejects.toThrow('Bridge interrupted.');
  expect(await chatbot.load_messages(conversationId)).toEqual(
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
    ((await chatbot.load_messages(conversationId)) as { status: string }[]).some(
      (message) => message.status === 'pending',
    ),
  ).toBe(false);
});
