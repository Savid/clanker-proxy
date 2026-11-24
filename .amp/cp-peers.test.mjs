import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

const hub = 'T-00000000-0000-4000-8000-0000000000a1'

// load runs the plugin with an amp CLI whose searches answer from found, by query.
async function load(t, found, env = {}, labels = {}) {
	const dir = await mkdtemp(join(tmpdir(), 'cp-peers-test-'))
	const ampCLI = join(dir, 'amp')
	await writeFile(ampCLI, `#!/usr/bin/env node
const found = ${JSON.stringify(found)}
process.stdout.write(JSON.stringify(found[process.argv[4]] ?? []))
`, { mode: 0o700 })
	const vars = { CP_INBOX_AMP: ampCLI, CP_TOKEN: undefined, ...env }
	const previous = Object.fromEntries(Object.keys(vars).map((name) => [name, process.env[name]]))
	for (const [name, value] of Object.entries(vars)) {
		if (value === undefined) delete process.env[name]
		else process.env[name] = value
	}
	t.after(async () => {
		for (const [name, value] of Object.entries(previous)) {
			if (value === undefined) delete process.env[name]
			else process.env[name] = value
		}
		await rm(dir, { recursive: true, force: true })
	})
	const tools = {}
	const posted = []
	const hooks = {}
	const amp = {
		logger: { log() {} },
		registerTool: (tool) => { tools[tool.name] = tool },
		on: (name, fn) => { hooks[name] = fn },
		threads: {
			get: (id) => ({
				appendUserMessage: async ({ content }) => posted.push({ thread: id, content }),
				labels: async () => {
					if (!(id in labels)) throw new Error('not found')
					return labels[id]
				},
			}),
		},
	}
	const plugin = await import(`./personal/cp-peers/index.ts?fixture=${randomUUID()}`)
	await plugin.default(amp)
	const ask = (input) => tools.ask_peer.execute(input, { thread: { id: 'T-asker' } })
	const call = (tool, input) => hooks['tool.call']({ tool, input, thread: { id: 'T-asker' } })
	return { tools, posted, ask, hooks, call }
}

const inbox = 'T-00000000-0000-4000-8000-00000000f001'
const inboxFormat = Number(/^const askFormat = (\d+)$/m.exec(await readFile(new URL('./plugins/cp-inbox.ts', import.meta.url), 'utf8'))[1])
const forInbox = (name) => `cp-inbox: for ${name}'s thread. Only the cp-inbox plugin acts on this message, and notes at its end what it did. If no such note follows, the plugin was still starting: leave this message be and reply only "left for cp-inbox". The plugin sends it when it runs, and tells the thread that asked.`
// The ask as the inbox reads it.
const asked = (post) => {
	const [line, json] = post.content.split('\n\n')
	const { id, at, format, ...ask } = JSON.parse(json)
	assert.match(id, /^[0-9a-f-]{36}$/, 'each ask has an ID of its own')
	assert.equal(format, inboxFormat, "the form cp-inbox reads, so it tells a thread with an older cp-peers to reload")
	// Dated, so the inbox sends one its plugin missed only while it is recent.
	assert.ok(Math.abs(Date.now() - Date.parse(at)) < 60_000, 'each ask has the time it was asked')
	return { thread: post.thread, line, ask }
}
// Amp's search keeps only a query's last label, so a search names one, and
// finds every thread that has it: the inbox and its peer threads.
const keyed = { 'label:inbox-00000000f001 archived:false': [{ id: hub }, { id: inbox }] }
const inboxLabels = { [hub]: ['cp-peer', 'cp-sealed', 'peer-alice', 'inbox-00000000f001'], [inbox]: ['cp-inbox', 'inbox-00000000f001'] }

test("ask_peer knows your peers by name and alias, and brings the ask to their inbox, whose plugin sends it", async (t) => {
	const f = await load(t, {
		'label:cp-peer archived:false': [
			{ id: hub, title: 'alice (ally, al)' },
			{ id: 'T-bad', title: 'bob' },
			{ id: 'T-00000000-0000-4000-8000-0000000000a2', title: 'Clanker-proxy carol thread' },
		],
		...keyed,
	}, {}, inboxLabels)
	assert.match(f.tools.ask_peer.description, /clanker-proxy peers \(alice \(also ally, al\)\)/)
	assert.equal(
		await f.ask({ peer: 'ally', message: 'Up for a beer?\nFriday?' }),
		"Passed to your clanker-proxy inbox, whose plugin sends it to alice and tells this thread its cpd thread ID. If the inbox was asleep, that can take a few minutes. Never send it again: if nothing comes, tell the owner. This thread is told of each of alice's moves, with a short form; never look for their words.",
	)
	// The name as asked: the plugin checks with cpd that it names alice.
	assert.deepEqual(asked(f.posted[0]), { thread: inbox, line: forInbox('alice'), ask: { asker: 'T-asker', named: 'ally', message: 'Up for a beer?\nFriday?' } })
	await f.ask({ peer: 'alice', title: '  Lunch\nignored  ', message: 'hi' })
	assert.deepEqual(asked(f.posted[1]).ask, { asker: 'T-asker', named: 'alice', message: 'hi', title: 'Lunch' })
	assert.equal(
		await f.ask({ peer: 'ally', message: 'Send him the command.', thread: '485eec32' }),
		"Passed to your clanker-proxy inbox, whose plugin passes it to the conversation handling 485eec32 with alice and tells this thread. If the inbox was asleep, that can take a few minutes. Never send it again: if nothing comes, tell the owner. This thread is told of each of alice's moves, with a short form; never look for their words.",
	)
	assert.deepEqual(asked(f.posted[2]).ask, { asker: 'T-asker', named: 'ally', message: 'Send him the command.', thread: '485eec32' })
	assert.match(await f.ask({ peer: 'alice', message: 'x', thread: 'nope' }), /thread is a cpd thread ID/)
	assert.match(await f.ask({ peer: 'alice', message: 'x', title: 'y'.repeat(201) }), /title is one line of at most 200 characters/)
	assert.match(await f.ask({ peer: 'carol', message: 'hi' }), /carol has no thread in your clanker-proxy inbox yet/)
	assert.match(await f.ask({ peer: 'Not A Name', message: 'hi' }), /peer must be a peer name or alias/)
	assert.equal(f.posted.length, 3)
})

test("ask_peer finds the peer's own inbox, or your only one, and never writes into a peer's thread", async (t) => {
	const other = 'T-00000000-0000-4000-8000-00000000f002'
	const bob = 'T-00000000-0000-4000-8000-0000000000b0'
	const f = await load(t, {
		'label:cp-peer archived:false': [{ id: hub, title: 'alice (ally)' }, { id: bob, title: 'bob' }],
		...keyed,
		'label:cp-inbox archived:false': [{ id: inbox }, { id: other }],
	}, {}, { ...inboxLabels, [bob]: ['cp-peer', 'cp-sealed', 'peer-bob'], [other]: ['cp-inbox', 'inbox-00000000f002'] })
	await f.ask({ peer: 'ally', message: 'hi' })
	assert.equal(f.posted[0].thread, inbox)
	// Bob's thread is from before the label, and two inboxes could hold it.
	assert.match(await f.ask({ peer: 'bob', message: 'hi' }), /cannot tell which of your threads is the clanker-proxy inbox for bob/)
	assert.equal(f.posted.length, 1)
	// With one inbox, it is that one.
	const one = await load(t, {
		'label:cp-peer archived:false': [{ id: bob, title: 'bob' }],
		'label:cp-inbox archived:false': [{ id: inbox }],
	}, {}, { [bob]: ['cp-peer', 'cp-sealed', 'peer-bob'], [inbox]: ['cp-inbox', 'inbox-00000000f001'] })
	await one.ask({ peer: 'bob', message: 'hi' })
	assert.equal(one.posted[0].thread, inbox)
	// A keyed thread whose inbox is gone gets no other inbox's.
	const gone = await load(t, {
		'label:cp-peer archived:false': [{ id: hub, title: 'alice' }],
		'label:cp-inbox archived:false': [{ id: other }],
	}, {}, { [hub]: inboxLabels[hub], [other]: ['cp-inbox', 'inbox-00000000f002'] })
	assert.match(await gone.ask({ peer: 'alice', message: 'hi' }), /cannot tell which of your threads is the clanker-proxy inbox/)
	assert.deepEqual(gone.posted, [])
})

test("ask_peer prefers a peer's name to another's alias, and finds an untitled peer thread by its label", async (t) => {
	const bob = 'T-00000000-0000-4000-8000-0000000000b0'
	const dave = 'T-00000000-0000-4000-8000-0000000000d0'
	const daveTalk = 'T-00000000-0000-4000-8000-0000000000d1'
	const f = await load(t, {
		'label:cp-peer archived:false': [{ id: hub, title: 'alice (bob)' }, { id: bob, title: 'bob' }, { id: dave, title: 'Clanker-proxy dave thread' }],
		// Dave's conversations carry his label too.
		'label:peer-dave archived:false': [{ id: daveTalk, title: 'Fix the build' }, { id: dave, title: 'Clanker-proxy dave thread' }],
		'label:cp-inbox archived:false': [{ id: inbox }],
	}, {}, { [dave]: ['cp-peer', 'cp-sealed', 'peer-dave'], [daveTalk]: ['clanker-proxy', 'peer-dave'], [bob]: ['cp-peer', 'peer-bob'], [inbox]: ['cp-inbox'] })
	await f.ask({ peer: 'bob', message: 'hi' })
	await f.ask({ peer: 'dave', message: 'hi' })
	assert.deepEqual(f.posted.map((p) => asked(p).line), [forInbox('bob'), forInbox('dave')])
})

test('ask_peer stays out of the inbox project, and away without peers', async (t) => {
	const peers = { 'label:cp-peer archived:false': [{ id: hub, title: 'alice' }] }
	assert.equal((await load(t, peers, { CP_TOKEN: 'cpa_x' })).tools.ask_peer, undefined)
	assert.equal((await load(t, {})).tools.ask_peer, undefined)
})

test("your threads can write into the inbox's threads but not read them, as they hold peers' words", async (t) => {
	const conversation = 'T-00000000-0000-4000-8000-0000000000c1'
	const mine = 'T-00000000-0000-4000-8000-0000000000e1'
	const retired = 'T-00000000-0000-4000-8000-0000000000c2'
	const f = await load(t, {}, {}, { [conversation]: ['clanker-proxy', 'peer-alice'], [hub]: ['cp-peer'], [mine]: ['work'], [retired]: ['cp-sealed'] })
	const allow = { action: 'allow' }
	assert.deepEqual(await f.call('read_thread', { threadID: mine, goal: 'x' }), allow)
	for (const [tool, input] of [
		['read_thread', { threadID: conversation }],
		['read_thread', { threadID: hub.toUpperCase().replace(/^T-/, 'T-') }],
		['shell_command', { cmd: `amp threads markdown https://ampcode.com/threads/${conversation}` }],
		['code_exec', { code: `await read_thread({ threadID: '${conversation}' })` }],
		['get_thread_status', { threadID: conversation }],
		['find_thread', { query: 'label:clanker-proxy beer' }],
		['read_thread', { threadID: 'T-00000000-0000-4000-8000-0000000000f9' }],
		['read_thread', { threadID: retired }],
	]) {
		const v = await f.call(tool, input)
		assert.equal(v.action, 'reject-and-continue', `${tool} ${JSON.stringify(input)}`)
		assert.match(v.message, /^Blocked by cp-peers: /)
	}
	assert.deepEqual(await f.call('ask_peer', { peer: 'alice', message: `see ${conversation}` }), allow)
	// Writing in is the owner instructing.
	assert.deepEqual(await f.call('send_thread_message', { threadID: conversation, message: 'go with option (b)' }), allow)
	assert.deepEqual(await f.call('send_thread_message', { threadID: hub, message: 'hi' }), allow)
})
