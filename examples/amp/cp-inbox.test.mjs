import assert from 'node:assert/strict'
import { createHmac, randomUUID } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

const subject = '00000000-0000-4000-8000-000000000001'
const peer = 'alice'
const key = Buffer.alloc(32, 7)

async function fixture(t) {
	const dir = await mkdtemp(join(tmpdir(), 'cp-inbox-test-'))
	const stateFile = join(dir, 'cp-inbox', 'state.json')
	const responseFile = join(dir, 'response.json')
	const cpctl = join(dir, 'cpctl')
	const env = { XDG_STATE_HOME: dir, CP_INBOX_CPCTL: cpctl, CP_WEBHOOK_SECRET: key.toString('base64') }
	const previous = Object.fromEntries(Object.keys(env).map((name) => [name, process.env[name]]))
	Object.assign(process.env, env)
	t.after(async () => {
		for (const [name, value] of Object.entries(previous)) {
			if (value === undefined) delete process.env[name]
			else process.env[name] = value
		}
		await rm(dir, { recursive: true, force: true })
	})
	await mkdir(join(dir, 'cp-inbox'))
	await writeFile(stateFile, JSON.stringify({
		seen: [], wakes: [], known: ['conversation'], lastPeeringNotice: 0, heldNotices: {},
		conversations: { [subject]: {
			amp: 'conversation', peer, created: Date.now(), briefed: true,
			wakes: [], replies: 0, lastReply: 0, held: false, ended: 0,
		} },
	}))
	await writeFile(cpctl, `#!/usr/bin/env node
const fs = require('node:fs')
const path = require('node:path')
const args = process.argv.slice(2)
if (args[0] !== '-json') process.exit(2)
if (args[1] === 'ls') {
    process.stdout.write('{"threads":[]}')
} else if (args[1] === 'show' && args[2] === ${JSON.stringify(subject)}) {
    const response = JSON.parse(fs.readFileSync(path.join(__dirname, 'response.json'), 'utf8'))
    if (response.error) {
        process.stderr.write(response.error)
        process.exit(1)
    }
    process.stdout.write(response.raw ?? JSON.stringify(response))
} else {
    process.exit(2)
}
`, { mode: 0o700 })
	const messages = []
	const logs = []
	let handler
	const conversation = {
		state: { get: async () => 'running' },
		appendUserMessage: async ({ content }) => messages.push(content),
	}
	const amp = {
		logger: { log: (...args) => logs.push(args) },
		threads: { get: () => conversation },
		on() {}, registerTool() {}, registerCommand() {},
		createWebhook: async (registration) => {
			handler = registration.handler
			return { url: 'https://example.invalid/webhook' }
		},
	}
	const plugin = await import(`./cp-inbox.ts?fixture=${randomUUID()}`)
	await plugin.default(amp)
	assert.equal(typeof handler, 'function')
	const ctx = { signal: new AbortController().signal, logger: amp.logger }
	return {
		messages, logs,
		readState: async () => JSON.parse(await readFile(stateFile, 'utf8')),
		respond: async (response) => writeFile(responseFile, JSON.stringify({ id: subject, peer, ...response })),
		event(type, state, myTurn = false) {
			const id = randomUUID()
			const timestamp = String(Math.floor(Date.now() / 1000))
			const body = Buffer.from(JSON.stringify({ id, type, state, myTurn, subject, peer, origin: 'incoming' }))
			const signature = createHmac('sha256', key).update(`${id}.${timestamp}.`).update(body).digest('base64')
			return { id, body, receivedAt: new Date().toISOString(), headers: {
				'webhook-id': id, 'webhook-timestamp': timestamp, 'webhook-signature': `v1,${signature}`,
			} }
		},
		deliver: (event) => handler(event, ctx),
	}
}

test('delayed close cannot end a conversation already reopened', async (t) => {
	const f = await fixture(t)
	await f.respond({ state: 'acked' })
	const close = f.event('thread.close', 'closed')
	await f.deliver(f.event('thread.reopen', 'acked', true))
	assert.equal(f.messages.length, 1)
	await f.deliver(close)
	assert.equal(f.messages.length, 1)
	const state = await f.readState()
	assert.equal(state.conversations[subject].ended, 0)
	assert.ok(state.seen.includes(close.id))
})

test('terminal notification checks the current thread and leaves the agent a race-safe instruction', async (t) => {
	const f = await fixture(t)
	for (const state of ['closed', 'declined', 'withdrawn']) {
		await f.respond({ state: 'acked' })
		await f.deliver(f.event('thread.reopen', 'acked', true))
		assert.equal((await f.readState()).conversations[subject].ended, 0)
		await f.respond({ state })
		await f.deliver(f.event('thread.close', 'closed'))
		assert.ok((await f.readState()).conversations[subject].ended > 0)
		assert.match(f.messages.at(-1), new RegExp(`cpctl show ${subject}`))
		assert.match(f.messages.at(-1), /Stop work only if it is still closed, declined or withdrawn/)
	}
	await f.respond({ state: 'acked', log: [{ body: 'a'.repeat(2 * 1024 * 1024) }] })
	await f.deliver(f.event('thread.reopen', 'acked', true))
	await f.deliver(f.event('thread.close', 'closed'))
	assert.equal(f.messages.length, 7)
	assert.equal((await f.readState()).conversations[subject].ended, 0)
})

test('unavailable or invalid current state is retried without leaking process output', async (t) => {
	const f = await fixture(t)
	const sentinel = 'private-output-must-not-escape'
	for (const response of [
		{ error: sentinel },
		{ raw: sentinel },
		{ raw: 'null' },
		{ state: 'closed', id: randomUUID() },
		{ state: 'closed', peer: 'mallory' },
		{ state: 'unknown' },
	]) {
		await f.respond(response)
		const event = f.event('thread.close', 'closed')
		await assert.rejects(f.deliver(event), (err) => {
			assert.equal(err.message, 'cp-inbox: cannot read current thread state')
			assert.equal(err.cause, undefined)
			assert.ok(!String(err.stack).includes(sentinel))
			return true
		})
		const state = await f.readState()
		assert.equal(state.conversations[subject].ended, 0)
		assert.ok(!state.seen.includes(event.id))
	}
	assert.equal(f.messages.length, 0)
	assert.ok(!JSON.stringify(f.logs).includes(sentinel))
})
