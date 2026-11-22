import assert from 'node:assert/strict'
import { createHmac, randomUUID } from 'node:crypto'
import { execFileSync, spawnSync } from 'node:child_process'
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { homedir, tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

const subject = '00000000-0000-4000-8000-000000000001'
const sent = '00000000-0000-4000-8000-000000000002'
const peer = 'alice'
const token = 'cpa_' + 'T'.repeat(43)
const hookURL = 'https://example.invalid/webhook'
// The key the plugin derives from its agent token and webhook URL.
const key = createHmac('sha256', token).update(`cp-inbox webhook\n${hookURL}`).digest()
// What the plugin sent cpd, by fixture.
const registrations = []

// fixture loads the plugin; base is where its home goes, by default with the rest.
async function fixture(t, base) {
	const dir = await mkdtemp(join(tmpdir(), 'cp-inbox-test-'))
	const stateFile = join(dir, 'cp-inbox', 'state.json')
	const responseFile = join(dir, 'response.json')
	const cpctl = join(dir, 'cpctl')
	const ampCLI = join(dir, 'amp')
	const home = join(base ?? dir, 'home')
	const checkout = join(home, 'workspace', 'repo')
	const env = { XDG_STATE_HOME: dir, CP_INBOX_CPCTL: cpctl, CP_INBOX_AMP: ampCLI, CP_URL: 'https://cpd.test/', CP_TOKEN: token, CP_INBOX_HOME: home, AMP_WORKING_DIRECTORY: checkout }
	const realFetch = globalThis.fetch
	globalThis.fetch = async (url, init) => {
		registrations.push({ url, init })
		return new Response('{}', { status: 200 })
	}
	t.after(() => { globalThis.fetch = realFetch })
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
	await mkdir(checkout, { recursive: true })
	await writeFile(stateFile, JSON.stringify({
		seen: [], wakes: [], known: ['conversation'], lastPeeringNotice: 0, heldNotices: {}, hubs: {},
		conversations: { [subject]: {
			amp: 'conversation', peer, created: Date.now(), briefed: true,
			wakes: [], replies: 0, lastReply: 0, held: false, ended: 0,
		} },
	}))
	await writeFile(cpctl, `#!/usr/bin/env node
const fs = require('node:fs')
const path = require('node:path')
const args = process.argv.slice(2)
if (args[0] !== '--json') process.exit(2)
if (args[1] === 'ls') {
    process.stdout.write('{"threads":[]}')
} else if (args[1] === 'send') {
    const body = fs.readFileSync(0, 'utf8')
    fs.writeFileSync(path.join(__dirname, 'sent.json'), JSON.stringify({ args: args.slice(2), body }))
    if (args[args.indexOf('--') + 1] === 'nobody') {
        process.stderr.write('{"error":{"message":"nobody is not a peer","exit":4,"status":404,"hint":"cpctl peer ls lists peers"}}')
        process.exit(4)
    }
    process.stdout.write(JSON.stringify({ id: ${JSON.stringify(sent)} }))
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
	// The amp CLI records each call, one JSON line per call.
	await writeFile(ampCLI, `#!/usr/bin/env node
const fs = require('node:fs')
const path = require('node:path')
const args = process.argv.slice(2)
fs.appendFileSync(path.join(__dirname, 'amp.log'), JSON.stringify(args) + '\\n')
if (args[1] === 'search') {
    const found = JSON.parse(fs.readFileSync(path.join(__dirname, 'search.json'), 'utf8').toString() || '{}')
    process.stdout.write(JSON.stringify(found[args[2]] ?? []))
}
`, { mode: 0o700 })
	await writeFile(join(dir, 'search.json'), '{}')
	const ampCalls = async () => (await readFile(join(dir, 'amp.log'), 'utf8').catch(() => '')).split('\n').filter(Boolean).map((l) => JSON.parse(l))
	let threadState = 'running'
	// Every message and the thread it went to; messages holds the texts.
	const messages = []
	const posted = []
	const logs = []
	// Threads the plugin started, with their parents.
	const created = []
	// The agents the plugin made, by their config.
	const agents = []
	const confirms = []
	let confirmAnswer = true
	let handler
	let guard
	let agentEnd
	let sessionStart
	const tools = {}
	const commands = {}
	// Quick actions, with the thread they were added to.
	const buttons = []
	const labels = {}
	// Threads whose messages fail, as an archived one's do.
	const failing = new Set()
	const handle = (id) => ({
		id,
		state: { get: async () => threadState },
		waitForResponse: async () => undefined,
		labels: async () => labels[id] ?? [],
		addLabels: async (l) => { labels[id] = [...(labels[id] ?? []), ...l] },
		setLabels: async (l) => { labels[id] = l },
		createQuickAction: (definition) => {
			buttons.push({ thread: id, ...definition })
			return { unsubscribe() {}, update() {} }
		},
		appendUserMessage: async ({ content }) => {
			if (failing.has(id)) throw new Error('archived')
			messages.push(content)
			posted.push({ thread: id, content })
		},
	})
	const notices = []
	const ui = {
		notify: async (message) => { notices.push(message) },
		confirm: async (c) => {
			confirms.push(c)
			return confirmAnswer
		},
	}
	const agent = {
		createThread: async ({ parentThreadID }) => {
			const id = `thread-${created.length + 1}`
			created.push({ id, parent: parentThreadID })
			return handle(id)
		},
	}
	const amp = {
		logger: { log: (...args) => logs.push(args) },
		threads: { get: handle },
		helpers: { shellCommandFromToolCall: (e) => (e.tool === 'shell_command' ? { command: e.input.cmd } : null) },
		on(name, fn) {
			if (name === 'tool.call') guard = fn
			if (name === 'agent.end') agentEnd = fn
			if (name === 'session.start') sessionStart = fn
		},
		registerTool(tool) {
			tools[tool.name] = tool
		},
		registerCommand(id, options, run) {
			commands[id] = run
		},
		createAgent: (config) => {
			agents.push(config)
			return agent
		},
		createWebhook: async (registration) => {
			handler = registration.handler
			return { url: hookURL }
		},
	}
	const plugin = await import(`./plugins/cp-inbox.ts?fixture=${randomUUID()}`)
	await plugin.default(amp)
	assert.equal(typeof handler, 'function')
	// Another thread's plugin instance, sharing the orb's state file.
	const another = async () => {
		const own = {}
		const instance = await import(`./plugins/cp-inbox.ts?fixture=${randomUUID()}`)
		await instance.default({ ...amp, on() {}, registerTool: (tool) => { own[tool.name] = tool } })
		return { send: (thread, input) => own.cp_send.execute(input, { thread: { id: thread }, ui, signal: new AbortController().signal }) }
	}
	const ctx = { signal: new AbortController().signal, logger: amp.logger, ui, thread: { id: 'inbox', agent: async () => agent, addLabels: async () => undefined } }
	return {
		home, checkout, own: join(home, 'cp-work', subject.slice(0, 8)),
		messages, posted, logs, created, agents, tools, confirms, buttons, notices,
		open: (thread) => sessionStart({ thread: { id: thread } }),
		command: (id, thread) => commands[id]({ ui, thread: thread && handle(thread) }), another, ampCalls, labels, failing,
		searches: (found) => writeFile(join(dir, 'search.json'), JSON.stringify(found)),
		idle: () => { threadState = 'idle' },
		end: (thread) => agentEnd({ thread: { id: thread } }),
		answerConfirm: (answer) => { confirmAnswer = answer },
		lastSent: async () => JSON.parse(await readFile(join(dir, 'sent.json'), 'utf8')),
		send: (thread, input) => tools.cp_send.execute(input, { thread: { id: thread }, ui, signal: new AbortController().signal }),
		check: (thread, tool, input) => guard({ thread: { id: thread }, tool, input }, { ui }),
		readState: async () => JSON.parse(await readFile(stateFile, 'utf8')),
		respond: async (response) => writeFile(responseFile, JSON.stringify({ id: subject, peer, ...response })),
		event(type, state, myTurn = false, about = subject, from = peer) {
			const id = randomUUID()
			const timestamp = String(Math.floor(Date.now() / 1000))
			const body = Buffer.from(JSON.stringify({ id, type, state, myTurn, subject: about, peer: from, origin: 'incoming' }))
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

test('an overruled move reaches only the conversation that made it', async (t) => {
	const f = await fixture(t)
	await f.deliver(f.event('thread.overruled', 'resolved'))
	assert.equal(f.messages.length, 1)
	assert.match(f.messages[0], /did not take effect: alice's move came first and the thread is now resolved/)
	assert.match(f.messages[0], new RegExp(`cpctl show ${subject}`))
	await f.deliver(f.event('thread.overruled', 'resolved', false, '00000000-0000-4000-8000-000000000002'))
	assert.equal(f.messages.length, 1, 'no conversation is started for it')
})

test('the inbox registers itself with cpd, signed with a key both sides derive', async (t) => {
	registrations.length = 0
	const f = await fixture(t)
	await new Promise((resolve) => setImmediate(resolve))
	const r = registrations.at(-1)
	assert.equal(r.url, 'https://cpd.test/api/v1/me/webhook')
	assert.equal(r.init.method, 'POST')
	assert.equal(r.init.headers.authorization, `Bearer ${token}`)
	assert.deepEqual(JSON.parse(r.init.body), { url: hookURL, secret: key.toString('base64') })
	// Events signed with that key are accepted.
	await f.respond({ state: 'acked' })
	await f.deliver(f.event('thread.reopen', 'acked', true))
	assert.equal(f.messages.length, 1)
})

test('each peer gets a thread under the inbox, and each of its cpd threads a conversation under that', async (t) => {
	const f = await fixture(t)
	const thread = (n) => `00000000-0000-4000-8000-00000000001${n}`
	await f.deliver(f.event('thread.open', 'open', true, thread(1)))
	assert.deepEqual(f.created, [{ id: 'thread-1', parent: 'inbox' }, { id: 'thread-2', parent: 'thread-1' }])
	assert.equal(f.posted[0].thread, 'thread-1')
	assert.match(f.posted[0].content, /my clanker-proxy thread for alice/)
	assert.equal(f.posted[1].thread, 'thread-2')
	assert.match(f.posted[1].content, new RegExp(`You handle clanker-proxy thread ${thread(1)} with alice`))
	await f.deliver(f.event('thread.open', 'open', true, thread(2)))
	assert.deepEqual(f.created.at(-1), { id: 'thread-3', parent: 'thread-1' }, "the peer's thread is reused")
	await f.deliver(f.event('thread.open', 'open', true, thread(3), 'bob'))
	assert.deepEqual(f.created.slice(-2), [{ id: 'thread-4', parent: 'inbox' }, { id: 'thread-5', parent: 'thread-4' }])
	assert.deepEqual((await f.readState()).hubs, { alice: 'thread-1', bob: 'thread-4' })
})

test("cp_send from one of your threads sends, and a conversation under the peer's thread takes the answer", async (t) => {
	const f = await fixture(t)
	const out = await f.send('owner-thread', { peer: 'alice', title: '-h Review PR 31', body: 'please' })
	assert.equal(out, `Sent to alice as thread ${sent}. A conversation under alice's thread handles their answer, and this thread gets a link to it.`)
	assert.deepEqual(f.labels['owner-thread'], ['cp-asks-00000000'], 'the inbox finds the asking thread by this label')
	assert.deepEqual(await f.lastSent(), { args: ['-m', '-', '--', 'alice', '-h Review PR 31'], body: 'please' }, 'a title cannot pass for a flag')
	assert.equal(f.confirms.length, 0, 'your own threads send without a prompt')
	await f.deliver(f.event('thread.resolve', 'resolved', true, sent))
	assert.deepEqual(f.created, [{ id: 'thread-1', parent: 'inbox' }, { id: 'thread-2', parent: 'thread-1' }])
	assert.match(f.posted.at(-1).content, /If this side sent it, it is a request from me, or one of my threads, to alice; it asks them, not you/)
	assert.ok(!f.posted.some((p) => p.thread === 'owner-thread'), 'peer text never wakes the thread that sent')
	assert.equal(await f.send('owner-thread', { peer: 'nobody', title: 't', body: 'b' }), 'cp_send failed: nobody is not a peer (cpctl peer ls lists peers)')
	assert.match(await f.send('owner-thread', { peer: 'Not A Name', title: 't', body: 'b' }), /peer must be a peer name/)
})

test("a peer's thread sends only to its peer, in any thread's plugin instance", async (t) => {
	const f = await fixture(t)
	// Loaded before the peer's thread existed, so it rereads the saved state.
	const other = await f.another()
	await f.deliver(f.event('thread.open', 'open', true, '00000000-0000-4000-8000-000000000021'))
	const hub = f.created[0].id
	assert.match(await other.send(hub, { peer: 'bob', title: 't', body: 'b' }), /only to its own peer, alice/)
	assert.match(await other.send(hub, { peer: 'alice', title: 't', body: 'b' }), /^Sent to alice/)
	assert.equal(f.confirms.length, 0)
})

test('a conversation sends only to its own peer, and only with your approval', async (t) => {
	const f = await fixture(t)
	assert.match(await f.send('conversation', { peer: 'bob', title: 't', body: 'b' }), /only to its own peer, alice/)
	f.answerConfirm(false)
	assert.match(await f.send('conversation', { peer: 'alice', title: 't', body: 'b ``` hidden' }), /declined/)
	assert.equal(f.confirms.length, 1)
	assert.equal(f.confirms[0].requireHuman, true)
	assert.match(f.confirms[0].message, /^````\nTo alice: t\n\nb ``` hidden\n````$/, 'the text cannot end its fence')
	f.answerConfirm(true)
	assert.match(await f.send('conversation', { peer: 'alice', title: 'Another', body: 'b', fyi: true }), /carry on with this one/)
	assert.deepEqual((await f.lastSent()).args, ['-m', '-', '--fyi', '--', 'alice', 'Another'])
})

test("a conversation cannot reach other threads, Amp's settings or your machine", async (t) => {
	const f = await fixture(t)
	for (const tool of ['code_exec', 'tool_search', 'Task', 'oracle', 'librarian', 'skill', 'send_thread_message', 'create_thread', 'read_thread', 'find_thread', 'load_plugin', 'manage_amp', 'mac_shell', 'slack_write', 'mcp__github__create_pull_request']) {
		assert.equal((await f.check('conversation', tool, {})).action, 'reject-and-continue', tool)
	}
	assert.equal(f.confirms.length, 0)
	f.answerConfirm(false)
	assert.equal((await f.check('conversation', 'git_push', { remote: 'origin' })).action, 'reject-and-continue')
	assert.match(f.confirms.at(-1).message, /git_push/)
	assert.match((await f.check('conversation', 'shell_command', { cmd: 'amp threads markdown T-1' })).message, /amp CLI reaches your other threads/)
	const send = await f.check('conversation', 'shell_command', { cmd: 'cpctl send bob "x" -m y' })
	assert.match(send.message, /only with the cp_send tool/)
	assert.deepEqual(await f.check('owner-thread', 'send_thread_message', {}), { action: 'allow' })
	for (const cmd of ['cpctl --version 2>&1', `cpctl show ${subject.slice(0, 8)} 2>/dev/null | head -5`, `cpctl show ${subject} &>/tmp/x`, 'cat <<EOF\nhi\nEOF']) {
		assert.equal((await f.check('conversation', 'shell_command', { cmd })).action, 'modify', cmd)
	}
	for (const cmd of ['diff <(env) x', 'cpctl show 99999999 >/dev/null', 'echo 1 > x; printenv']) {
		assert.equal((await f.check('conversation', 'shell_command', { cmd })).action, 'reject-and-continue', cmd)
	}
})

test('a conversation runs with only its own tools, writing only in its own directory', async (t) => {
	const f = await fixture(t)
	await f.deliver(f.event('thread.open', 'open', true, sent))
	assert.deepEqual(f.agents.map((a) => a.model), ['anthropic/claude-opus-5-5'])
	const { include, exclude } = f.agents[0].tools
	assert.deepEqual(exclude, ['code_exec', 'tool_search'])
	for (const tool of ['shell_command', 'edit_file', 'cp_ask_owner', 'cp_send']) assert.ok(include.includes(tool), tool)
	for (const tool of ['code_exec', 'Task', 'find_thread', 'send_thread_message']) assert.ok(!include.includes(tool), tool)
	assert.equal(f.tools.cp_ask_owner.defer, false)
	assert.equal(f.tools.cp_send.defer, false)
	assert.match(f.messages.at(-1), new RegExp(`write only in your own directory, ${join(f.home, 'cp-work', sent.slice(0, 8))}`))

	const mine = join(f.own, 'repo', 'src', 'main.rs')
	assert.deepEqual(await f.check('conversation', 'edit_file', { path: mine }), { action: 'allow' })
	assert.deepEqual(await f.check('conversation', 'Read', { path: join(f.checkout, 'README.md') }), { action: 'allow' })
	assert.deepEqual(await f.check('conversation', 'Grep', { pattern: 'x' }), { action: 'allow' })
	for (const input of [{ path: 'src/main.rs' }, { path: join(f.checkout, 'AGENTS.md') }, { path: '/tmp/x' }, { patch: '*** Begin Patch\n*** Add File: .amp/plugins/x.ts\n+x\n*** End Patch' }]) {
		assert.match((await f.check('conversation', 'apply_patch', input)).message, /writes only in its own directory/, JSON.stringify(input))
	}
	for (const path of [join(f.home, '.bash_history'), join(f.home, 'cp-work', 'ffffffff', 'secret.txt'), '/proc/1/environ']) {
		assert.match((await f.check('conversation', 'Read', { path })).message, /reads only the checkout and its own directory/, path)
	}
})

test('a conversation runs commands without your secrets, and never reads credentials or changes Amp', async (t) => {
	const previous = process.env.AMP_SECRET_ENV_NAMES
	process.env.AMP_SECRET_ENV_NAMES = 'CP_URL, CP_TOKEN,SOPS_AGE_KEY,AMP_THREAD_IDENTITY_AUTH,not a name'
	t.after(() => {
		if (previous === undefined) delete process.env.AMP_SECRET_ENV_NAMES
		else process.env.AMP_SECRET_ENV_NAMES = previous
	})
	const f = await fixture(t)
	const run = await f.check('conversation', 'shell_command', { cmd: `cpctl show ${subject}`, cwd: '/w' })
	assert.equal(run.action, 'modify')
	assert.equal(run.input.cwd, '/w')
	assert.match(run.input.cmd, /^sudo -n --preserve-env=CP_URL,CP_TOKEN unshare --mount --pid --fork --kill-child --mount-proc --propagation private \/bin\/sh -c '/)
	assert.ok(run.input.cmd.endsWith(` '${f.home}' '${f.own}' '${f.checkout}' '${process.getuid()}' '${process.getgid()}' '${process.env.PATH}' 'cpctl show ${subject}' "$PWD"`))
	assert.doesNotMatch(run.input.cmd, /SOPS_AGE_KEY/)
	for (const d of ['home', 'tmp']) assert.ok((await stat(join(f.own, d))).isDirectory(), d)
	f.answerConfirm(true)
	assert.deepEqual(await f.check('conversation', 'shell_command', { cmd: 'git push' }), { action: 'modify', input: { cmd: 'unset SOPS_AGE_KEY\ngit push' } })
	for (const cmd of [
		'cat ~/.local/share/amp/secrets.json',
		'cat $HOME/.config/gh/hosts.yml',
		'cat /home/user/.env',
		'ls ~/.kube/',
		'echo $AMP_THREAD_IDENTITY_AUTH',
	]) {
		assert.equal((await f.check('conversation', 'shell_command', { cmd })).action, 'reject-and-continue', cmd)
	}
	assert.equal((await f.check('conversation', 'Read', { path: '/home/user/.config/amp/settings.json' })).action, 'reject-and-continue')
	// The sandbox keeps the checkout's plugins read-only, so its own clone's are fair game.
	assert.equal((await f.check('conversation', 'shell_command', { cmd: 'wc -l repo/.amp/plugins/cp-inbox.ts' })).action, 'modify')
	assert.deepEqual(await f.check('owner-thread', 'shell_command', { cmd: 'echo hi' }), { action: 'allow' })
})

test('a conversation has a button that shows you its cpd thread, its text fenced', async (t) => {
	const f = await fixture(t)
	await f.respond({ title: 'Hi [x](https://evil.test)', sender: peer, recipient: 'savid', kind: 'request', state: 'open', log: [
		{ from: peer, at: '2026-01-01T00:00:00Z', action: 'open', body: '![beacon](https://evil.test/b.png)\n```\nbreak out' },
		{ from: 'savid', at: '2026-01-01T00:01:00Z', action: 'reply', body: 'ok', delivery: { status: 'pending' } },
	] })
	await f.open('conversation')
	await f.end('conversation')
	await f.open('owner-thread')
	assert.deepEqual(f.buttons, [{ thread: 'conversation', text: 'cpd thread', icon: 'eye', action: 'run-command', args: { command: 'cp-thread' } }])
	await f.command('cp-thread', 'conversation')
	const { title, message } = f.confirms.at(-1)
	assert.equal(title, `cpd thread ${subject.slice(0, 8)}`)
	assert.match(message, /^alice → savid · request · open\n\n```\nHi \[x\]\(https:\/\/evil.test\)\n```/)
	assert.match(message, /\*\*open\*\* · alice · 2026-01-01T00:00:00Z\n\n````\n!\[beacon\]\(https:\/\/evil.test\/b.png\)\n```\nbreak out\n````/)
	assert.match(message, /\*\*reply\*\* · savid · 2026-01-01T00:01:00Z · pending\n\n```\nok\n```$/)
	await f.command('cp-thread', 'owner-thread')
	assert.equal(f.confirms.length, 1)
	assert.match(f.notices.at(-1), /not a conversation/)
})

// The sandbox itself, run for real where sudo needs no password, as in CI
// and Amp's orbs.
const sandboxable = spawnSync('sudo', ['-n', 'true']).status === 0 && spawnSync('sh', ['-c', 'command -v unshare && command -v setpriv']).status === 0
test('a sandboxed command sees only the checkout and its own directory, as you, without secrets or sudo', { skip: !sandboxable && 'needs passwordless sudo, unshare and setpriv' }, async (t) => {
	// Not under /tmp, which the sandbox replaces.
	const base = await mkdtemp(join(homedir(), '.cp-inbox-test-'))
	t.after(() => rm(base, { recursive: true, force: true }))
	const f = await fixture(t, base)
	const { home, checkout, own } = f
	const bin = join(home, '.local', 'bin')
	for (const d of [bin, join(home, '.amp', 'bin'), join(home, '.local', 'share', 'amp'), join(home, 'cp-work', 'ffffffff'), own]) await mkdir(d, { recursive: true })
	await writeFile(join(home, '.local', 'share', 'amp', 'secrets.json'), 'amp-secret')
	await writeFile(join(home, 'cp-work', 'ffffffff', 'other.txt'), 'other conversation')
	await writeFile(join(home, '.gitconfig'), '[user]\n\tname = Owner\n')
	await writeFile(join(checkout, 'AGENTS.md'), 'guidance')
	await writeFile(join(bin, 'tool'), '#!/bin/sh\necho tool-ran\n', { mode: 0o755 })
	// The agent's script: what the sandbox shows it.
	await writeFile(join(own, 'probe.sh'), `
see() { if [ -e "$1" ]; then echo "$2=seen"; else echo "$2=hidden"; fi; }
can() { if (echo x >> "$1") 2>/dev/null; then echo "$2=writable"; else echo "$2=read-only"; fi; }
echo "uid=$(id -u) groups=$(id -G)"
see ${home}/.local/share/amp/secrets.json amp
see ${home}/cp-work/ffffffff/other.txt other
see ${home}/.amp/bin amp-bin
cat ${checkout}/AGENTS.md; echo
can ${checkout}/AGENTS.md checkout
can ${own}/probe.out own
can /tmp/probe.out tmp
echo "home=$HOME"
tool
echo "secret=\${SOPS_AGE_KEY:-none} url=$CP_URL"
if sudo -n true 2>/dev/null; then echo sudo=yes; else echo sudo=no; fi
n=$(ls /proc | grep -c '^[0-9][0-9]*$'); if [ "$n" -lt 10 ]; then echo procs=few; else echo "procs=$n"; fi
git config --global user.name
`)
	const path = process.env.PATH
	process.env.PATH = `${bin}:${join(home, '.amp', 'bin')}:${path}`
	t.after(() => { process.env.PATH = path })
	const run = await f.check('conversation', 'shell_command', { cmd: 'sh probe.sh' })
	assert.equal(run.action, 'modify')
	const out = execFileSync('bash', ['-c', run.input.cmd], { cwd: own, encoding: 'utf8', env: { ...process.env, SOPS_AGE_KEY: 'leaked' } })
	const uid = process.getuid()
	assert.equal(out, [
		`uid=${uid} groups=${process.getgid()}`,
		'amp=hidden', 'other=hidden', 'amp-bin=hidden',
		'guidance', 'checkout=read-only', 'own=writable', 'tmp=writable',
		`home=${join(own, 'home')}`, 'tool-ran', 'secret=none url=https://cpd.test/', 'sudo=no', 'procs=few', 'Owner', '',
	].join('\n'))
})

test("a peer's thread is named after the peer once it has answered", async (t) => {
	const f = await fixture(t)
	await f.deliver(f.event('thread.open', 'open', true, '00000000-0000-4000-8000-000000000031'))
	for (let i = 0; i < 100 && !(await f.ampCalls()).length; i++) await new Promise((resolve) => setTimeout(resolve, 20))
	assert.deepEqual(await f.ampCalls(), [['threads', 'rename', 'thread-1', 'alice']])
})

test('a conversation is archived when its thread ends, and comes back with it', async (t) => {
	const f = await fixture(t)
	const ampCalls = f.ampCalls
	f.ampCalls = async () => (await ampCalls()).filter((c) => c[1] !== 'search')
	f.idle()
	await f.respond({ state: 'closed' })
	await f.deliver(f.event('thread.close', 'closed'))
	assert.deepEqual(await f.ampCalls(), [['threads', 'archive', 'conversation']], 'ended by the peer while idle')
	assert.equal(f.messages.length, 0)

	await f.respond({ state: 'acked' })
	await f.deliver(f.event('thread.reopen', 'acked', true))
	assert.deepEqual((await f.ampCalls()).at(-1), ['threads', 'archive', '--unarchive', 'conversation'])
	assert.equal(f.messages.length, 1, 'unarchived before the message')

	assert.deepEqual(await f.end('conversation'), { action: 'done' })
	assert.equal((await f.ampCalls()).length, 2, 'not archived while the thread is live')
	await f.respond({ state: 'closed' })
	await f.end('conversation')
	assert.deepEqual((await f.ampCalls()).at(-1), ['threads', 'archive', 'conversation'], 'ended by this side')
	await f.end('owner-thread')
	assert.equal((await f.ampCalls()).length, 3, 'other threads are never archived')
})

test('the thread that asked gets a link to the conversation once the peer answers, never their words', async (t) => {
	const f = await fixture(t)
	const asker = 'T-00000000-0000-4000-8000-0000000000aa'
	await f.searches({ 'label:cp-asks-00000000': [{ id: asker }] })
	f.labels[asker] = ['cp-asks-00000000', 'mine']
	await f.deliver(f.event('thread.ack', 'acked', false, sent))
	assert.ok(!f.posted.some((p) => p.thread === asker), 'an ack is not an answer')
	await f.deliver(f.event('thread.resolve', 'resolved', true, sent))
	const told = f.posted.filter((p) => p.thread === asker)
	assert.equal(told.length, 1)
	const conversation = f.created.at(-1).id
	assert.equal(told[0].content, `clanker-proxy: alice answered the thread you asked for, 00000000, in the conversation that handles it: https://ampcode.com/threads/${conversation}.`)
	assert.deepEqual(f.labels[asker], ['mine'], 'told once: the label goes')
})

test("a peer's thread sends for the thread that asked, which gets the link", async (t) => {
	const f = await fixture(t)
	await f.deliver(f.event('thread.open', 'open', true, '00000000-0000-4000-8000-000000000041'))
	const hub = f.created[0].id
	const asker = 'T-00000000-0000-4000-8000-0000000000bb'
	assert.match(await f.send(hub, { peer: 'alice', title: 't', body: 'b', replyTo: asker }), new RegExp(`thread ${asker} gets a link to it\\.$`))
	assert.deepEqual(f.labels[asker], ['cp-asks-00000000'])
	const out = await f.send(hub, { peer: 'alice', title: 't', body: 'b', replyTo: 'not a thread' })
	assert.match(out, /handles their answer\.$/)
	assert.ok(!Object.entries(f.labels).some(([id, l]) => id !== asker && l.some((x) => x.startsWith('cp-asks-'))), 'no other thread is told')
})

test('an ended thread never starts a new conversation', async (t) => {
	const f = await fixture(t)
	// The conversation was archived as it stopped, just before the close came.
	f.failing.add('conversation')
	await f.respond({ state: 'closed' })
	await f.deliver(f.event('thread.close', 'closed'))
	await f.deliver(f.event('thread.reply', 'closed'))
	await f.deliver(f.event('thread.reply', 'closed', false, '00000000-0000-4000-8000-000000000051'))
	assert.deepEqual(f.created, [])
	assert.equal((await f.readState()).conversations[subject].amp, 'conversation', 'kept, for a reopen to bring back')
	f.failing.delete('conversation')
	await f.respond({ state: 'acked' })
	await f.deliver(f.event('thread.reopen', 'acked', true))
	assert.equal(f.posted.at(-1).thread, 'conversation')
})
