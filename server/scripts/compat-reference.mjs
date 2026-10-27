#!/usr/bin/env node
// Records what a real Navidrome and a real Jellyfin answer for the compat
// requests DroppedNeedle is tested against, over six of the repo's fixture
// tracks. The captures live in server/tests/reference/compat; the README
// there says how to run this.
//
//   plant <reference dir> <tmp dir> [--set lyrics]   copy the fixture tracks into <tmp dir>/music
//   navidrome <reference dir> --image-id <id> [--port 14533] [--set arpeggi|ratings]
//   jellyfin <reference dir> --image-id <id> [--port 18096] [--set lyrics|played|paging]
//   check <reference dir>                verify the checksums and the scrubbing
//   clean <tmp dir>                      remove the planted tracks
//
// The script never starts a container or any other process. It talks to the
// loopback address only, refuses a server that is already set up, refuses
// any version but the pinned one, refuses to run in CI and never replaces a
// file.

import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HOST = '127.0.0.1';
const SERVERS = {
	navidrome: { image: 'deluan/navidrome:0.62.0', version: '0.62.0', port: 14533 },
	jellyfin: { image: 'jellyfin/jellyfin:10.10.6', version: '10.10.6', port: 18096 }
};
// Request sets recorded on their own, each into `<server>-<version>-<set>`.
const SETS = { navidrome: ['arpeggi', 'ratings'], jellyfin: ['lyrics', 'played', 'paging'] };
// Files a set plants beside the tracks, from the fixtures folder.
const EXTRA_FILES = {
	lyrics: [{ fixture: 'lyrics_airbag.lrc', path: 'Radiohead/OK Computer/01 Airbag.lrc' }]
};
const USER = 'reference';
const CLIENT = 'dn-ref';
const UNKNOWN_HEX_ID = 'ffffffffffffffffffffffffffffffff';
const SCRIPT = 'server/scripts/compat-reference.mjs';
const FIXTURES = path.resolve(
	path.dirname(fileURLToPath(import.meta.url)),
	'../tests/fixtures/library'
);

const KEPT_HEADERS = [
	'accept-ranges',
	'cache-control',
	'content-disposition',
	'content-encoding',
	'content-length',
	'content-range',
	'content-type',
	'etag',
	'last-modified',
	'transfer-encoding',
	'www-authenticate',
	'x-content-duration',
	'x-total-count'
];

class Stop extends Error {}

function stop(message) {
	throw new Stop(message);
}

function sleep(ms) {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

function sha256(data) {
	return crypto.createHash('sha256').update(data).digest('hex');
}

function refuseInCi() {
	if (process.env.CI)
		stop('refusing to run with CI set: captures are recorded by hand, once per server version');
}

function loadLibrary(dir) {
	const file = path.join(dir, 'library.json');
	const raw = fs.readFileSync(file);
	return { ...JSON.parse(raw.toString('utf8')), sha256: sha256(raw) };
}

// ---------------------------------------------------------------- plant, clean

function tmpDir(dir) {
	const resolved = path.resolve(dir);
	if (path.basename(resolved) !== 'dn-ref-tmp') stop('the tmp dir must be named dn-ref-tmp');
	return resolved;
}

function extraFiles(set) {
	if (set !== undefined && !Object.values(SETS).flat().includes(set)) stop(`unknown --set ${set}`);
	return EXTRA_FILES[set] ?? [];
}

function plant(refDir, dir, args) {
	refuseInCi();
	const tmp = tmpDir(dir);
	const library = loadLibrary(refDir);
	const extras = extraFiles(args.set);
	if (fs.existsSync(tmp)) stop(`${tmp} exists: run clean first`);
	for (const file of [...library.tracks, ...extras]) {
		const target = path.join(tmp, 'music', file.path);
		fs.mkdirSync(path.dirname(target), { recursive: true });
		fs.copyFileSync(path.join(FIXTURES, file.fixture), target, fs.constants.COPYFILE_EXCL);
	}
	console.log(
		`planted ${library.tracks.length} tracks and ${extras.length} other files under ${path.join(tmp, 'music')}`
	);
}

function clean(dir) {
	refuseInCi();
	fs.rmSync(tmpDir(dir), { recursive: true, force: true });
}

// ---------------------------------------------------------------- scrubbing

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const OPAQUE = /[A-Za-z0-9]{16,}/g;
const IPV4 = /(?<![\d.])\d{1,3}(?:\.\d{1,3}){3}(?![\d.])/g;
const URL_HOST = /https?:\/\/[^/\s"\\?]+/g;
const TOKEN_KEY = /^(accesstoken|token|api_key|apikey|subsonictoken|subsonicsalt|pw|password)$/i;
// Navidrome signs its image links with one.
const JWT = /eyJ[A-Za-z0-9_-]{10,}(?:\.[A-Za-z0-9_-]+)*/g;
const FORBIDDEN = [
	['a JWT', /eyJ/],
	['an IPv4 address', new RegExp(IPV4.source)],
	['a host path', /\/(srv|home|tmp)\//]
];

function isIdKey(key) {
	if (/musicbrainz/i.test(key) || /ProviderIds?$/.test(key)) return false;
	return /^(id|parent|coverArt|etag)$/i.test(key) || /Ids?$/.test(key) || /ImageTags?$/.test(key);
}

function isTimestampKey(key) {
	if (key === 'PremiereDate') return false;
	return (
		/^(created|starred|changed|played|lastScan|lastModified)$/.test(key) ||
		/Date$|^Date[A-Z]|CheckIn$/.test(key)
	);
}

class Scrubber {
	constructor() {
		this.secrets = [];
		this.ids = new Map();
		this.variants = new Map();
		this.left = new Set();
	}

	secret(value, placeholder) {
		if (typeof value === 'string' && value.length >= 8) this.secrets.push([value, placeholder]);
	}

	// A placeholder with the length and punctuation of the id it replaces,
	// numbered by first appearance. The dashed and plain forms of one UUID
	// share a number.
	id(raw) {
		const canonical = UUID.test(raw) ? raw.replaceAll('-', '').toLowerCase() : raw;
		if (!this.ids.has(canonical)) this.ids.set(canonical, this.ids.size + 1);
		if (!this.variants.has(raw)) {
			const width = raw.replace(/[^A-Za-z0-9]/g, '').length;
			const digits = String(this.ids.get(canonical)).padStart(width, '0');
			let next = 0;
			this.variants.set(
				raw,
				raw.replace(/[A-Za-z0-9]/g, () => digits[next++])
			);
		}
		return this.variants.get(raw);
	}

	idString(key, value) {
		if (UUID.test(value)) return this.id(value);
		// Navidrome cover ids end in the hex time the art last changed.
		const scrubbed = value
			.replace(OPAQUE, (token) => this.id(token))
			.replace(/_[0-9a-f]{6,12}$/, (stamp) => '_' + '0'.repeat(stamp.length - 1));
		if (scrubbed === value && value !== '') this.left.add(`${key}=${value}`);
		return scrubbed;
	}

	idValue(key, value) {
		if (typeof value === 'string') return this.idString(key, value);
		if (Array.isArray(value)) return value.map((item) => this.idValue(key, item));
		if (value && typeof value === 'object') {
			return Object.fromEntries(
				Object.entries(value).map(([k, v]) => [
					k,
					isIdKey(k) || typeof v !== 'object' ? this.idValue(k, v) : this.walk(v)
				])
			);
		}
		return value;
	}

	walk(value, key = '') {
		if (Array.isArray(value)) return value.map((item) => this.walk(item, key));
		if (value && typeof value === 'object') {
			return Object.fromEntries(
				Object.entries(value).map(([k, v]) => {
					if (typeof v === 'string' && TOKEN_KEY.test(k)) return [k, '<token>'];
					if (isTimestampKey(k) && typeof v === 'string') return [k, v.replace(/\d/g, '0')];
					if (isTimestampKey(k) && typeof v === 'number') return [k, 0];
					if (isIdKey(k)) return [k, this.idValue(k, v)];
					if (k === 'Key' && typeof v === 'string' && UUID.test(v)) return [k, this.id(v)];
					return [k, this.walk(v, k)];
				})
			);
		}
		return value;
	}

	// Applied to serialized text, so values are caught inside URLs too.
	text(input) {
		let out = input;
		for (const [value, placeholder] of this.secrets) out = out.replaceAll(value, placeholder);
		const ids = [...this.variants.entries()].sort((a, b) => b[0].length - a[0].length);
		for (const [raw, placeholder] of ids) out = out.replaceAll(raw, placeholder);
		return out
			.replace(JWT, '<token>')
			.replace(URL_HOST, 'http://reference.invalid')
			.replace(IPV4, 'reference.invalid');
	}

	verify(name, text) {
		for (const [value] of this.secrets)
			if (text.includes(value)) stop(`${name}: a secret survived scrubbing`);
		for (const raw of this.variants.keys())
			if (text.includes(raw)) stop(`${name}: a server id survived scrubbing`);
		for (const [what, pattern] of FORBIDDEN) if (pattern.test(text)) stop(`${name}: holds ${what}`);
	}
}

// ---------------------------------------------------------------- recording

class Recorder {
	constructor(server, port) {
		this.server = server;
		this.base = `http://${HOST}:${port}`;
		this.refs = {};
		this.scrub = new Scrubber();
		this.folders = new Map();
		this.current = null;
		this.authorize = (row) => row;
	}

	fill(template) {
		if (Array.isArray(template)) return template.map((item) => this.fill(item));
		if (template && typeof template === 'object') {
			return Object.fromEntries(Object.entries(template).map(([k, v]) => [k, this.fill(v)]));
		}
		if (typeof template !== 'string') return template;
		return template.replace(/\{([a-z0-9_.]+)\}/g, (_, key) => {
			if (!(key in this.refs)) stop(`nothing on the server matches {${key}}`);
			return this.refs[key];
		});
	}

	// Sends one request. Templates stay in the log so no secret is printed.
	async send(row) {
		const { query = [], headers = {} } = this.authorize(row);
		const search = query
			.map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(this.fill(v))}`)
			.join('&');
		const target = this.fill(row.path) + (search ? `?${search}` : '');
		const method = row.method ?? 'GET';
		const init = {
			method,
			headers: { 'accept-encoding': 'identity', ...this.fill(headers) },
			redirect: 'manual',
			signal: AbortSignal.timeout(60_000)
		};
		if (row.body !== undefined) {
			init.body = JSON.stringify(this.fill(row.body));
			init.headers['content-type'] = 'application/json';
		}
		console.error(`-> ${method} ${row.path}`);
		const res = await fetch(this.base + target, init);
		const answer = { request: `${method} ${target}`, status: res.status, headers: {} };
		for (const name of KEPT_HEADERS) {
			const value = res.headers.get(name);
			if (value === null) continue;
			answer.headers[name] =
				name === 'etag' ? '<etag>' : name === 'last-modified' ? '<date>' : value;
		}
		const type = res.headers.get('content-type') ?? '';
		if (row.read === 'headers') {
			await res.body?.cancel();
			return { ...answer, kind: 'unread', body: { unread: true } };
		}
		const bytes = Buffer.from(await res.arrayBuffer());
		if (/json/.test(type)) {
			return { ...answer, kind: 'json', body: JSON.parse(bytes.toString('utf8')) };
		}
		if (row.read === 'bytes' && !/^text\//.test(type)) {
			return { ...answer, kind: 'bytes', body: { bytes: bytes.length } };
		}
		if (bytes.length === 0) return { ...answer, kind: 'empty', body: null };
		return { ...answer, kind: 'text', body: { text: bytes.toString('utf8') } };
	}

	// Polls until the server answers 200. This is the first read of a run.
	async firstRead(row) {
		for (let attempt = 0; attempt < 180; attempt++) {
			try {
				const answer = await this.send(row);
				if (answer.status === 200) return answer;
			} catch (error) {
				if (error instanceof Stop) throw error;
			}
			await sleep(1000);
		}
		stop(`no answer from ${this.base}`);
	}

	async ok(row) {
		const answer = await this.send(row);
		if (answer.status < 200 || answer.status > 299)
			stop(`${row.method ?? 'GET'} ${row.path} answered ${answer.status}`);
		return answer;
	}

	into(folder) {
		this.current = [];
		this.folders.set(folder, this.current);
	}

	async record(row) {
		const answer = await this.send(row);
		if (row.sets) {
			const value = row.sets.at.reduce((node, key) => node?.[key], answer.body);
			if (typeof value !== 'string')
				stop(`${row.name}: the answer has no ${row.sets.at.join('.')}`);
			this.refs[row.sets.ref] = value;
			this.scrub.id(value);
		}
		this.current.push({ row, answer });
		return answer;
	}

	async recordAll(rows) {
		for (const row of rows) await this.record(row);
	}

	// Nothing is written until every file of the run has passed the checks.
	write(capture) {
		const { scrub } = this;
		for (const [key, value] of Object.entries(this.refs)) {
			if (key === 'password' || key === 'token' || key === 'user_name') continue;
			if (value.length < 16) stop(`{${key}} is too short to replace safely`);
			scrub.id(value);
		}
		const walked = [...this.folders].map(([folder, steps]) => [
			folder,
			steps.map(({ row, answer }) => ({
				row,
				answer,
				body: answer.kind === 'json' ? scrub.walk(answer.body) : answer.body
			}))
		]);
		const out = [];
		for (const [folder, steps] of walked) {
			const files = new Map();
			const index = [];
			for (const { row, answer, body } of steps) {
				const step = {
					request: answer.request,
					status: answer.status,
					headers: answer.headers,
					body
				};
				const file = `${row.name}.json`;
				if (files.has(file)) stop(`two rows are named ${row.name}`);
				files.set(file, scrub.text(JSON.stringify(step, null, 2)) + '\n');
				index.push({
					name: row.name,
					file,
					...(row.write ? { write: true } : {}),
					method: row.method ?? 'GET',
					path: row.path,
					query: row.query ?? [],
					...(row.headers ? { headers: row.headers } : {}),
					...(row.body !== undefined ? { body: row.body } : {}),
					auth: row.auth ?? 'default',
					answer: answer.kind,
					...(row.sets ? { sets: row.sets } : {})
				});
			}
			files.set('index.json', JSON.stringify({ requests: index }, null, 2) + '\n');
			files.set('capture.json', JSON.stringify(capture, null, 2) + '\n');
			for (const [name, text] of files) scrub.verify(`${path.basename(folder)}/${name}`, text);
			const sums = [...files]
				.map(([name, text]) => `${sha256(text)}  ${name}\n`)
				.sort((a, b) => (a.slice(66) < b.slice(66) ? -1 : 1));
			files.set('SHA256SUMS', sums.join(''));
			out.push([folder, files]);
		}
		for (const [folder] of out) if (fs.existsSync(folder)) stop(`${folder} exists`);
		for (const [folder, files] of out) {
			fs.mkdirSync(folder);
			for (const [name, text] of files)
				fs.writeFileSync(path.join(folder, name), text, { flag: 'wx' });
			console.log(`${path.basename(folder)}: ${files.size - 3} requests`);
		}
		console.log(`ids replaced: ${scrub.ids.size}`);
		if (scrub.left.size)
			console.log(`id-like keys left as they were:\n  ${[...scrub.left].sort().join('\n  ')}`);
	}
}

function prepare(server, refDir, args) {
	refuseInCi();
	const pinned = SERVERS[server];
	const imageId = args['image-id'];
	if (!/^sha256:[0-9a-f]{64}$/.test(imageId ?? ''))
		stop('--image-id sha256:... is required (from docker container inspect)');
	const port = Number(args.port ?? pinned.port);
	if (!Number.isInteger(port) || port < 1024 || port > 65535) stop('bad --port');
	const set = args.set;
	if (set !== undefined && !(SETS[server] ?? []).includes(set)) stop(`unknown --set ${set}`);
	const library = loadLibrary(refDir);
	const folder = path.join(path.resolve(refDir), `${server}-${pinned.version}`);
	const folders = set ? [`${folder}-${set}`] : [folder, `${folder}-writes`];
	for (const dir of folders)
		if (fs.existsSync(dir)) stop(`${dir} exists: a capture is never replaced`);
	const capture = (reported) => ({
		server,
		image: pinned.image,
		image_id: imageId,
		reported_version: reported,
		recorded: new Date().toISOString().slice(0, 10),
		recorder: SCRIPT,
		library: '../library.json',
		library_sha256: library.sha256,
		...(extraFiles(set).length ? { extra_files: extraFiles(set) } : {})
	});
	return { pinned, library, folders, set, capture, recorder: new Recorder(server, port) };
}

function resolveByName(recorder, kind, wanted, found) {
	for (const { key, name } of wanted) {
		const matches = found.filter((item) => item.name === name);
		if (matches.length > 1) stop(`${matches.length} ${kind}s are named ${name}`);
		if (matches.length === 1) recorder.refs[`${kind}.${key}`] = matches[0].id;
		else console.error(`no ${kind} named ${name} on this server`);
	}
}

// A server may split one tagged album in two, so an album is the one that
// holds the first track library.json lists for it.
function resolveTracks(recorder, library, found) {
	resolveByName(
		recorder,
		'track',
		library.tracks.map((t) => ({ key: t.key, name: t.title })),
		found
	);
	for (const track of library.tracks) {
		const match = found.find((item) => item.id === recorder.refs[`track.${track.key}`]);
		if (!match?.album) stop(`${track.title} is missing or has no album on this server`);
		recorder.refs[`album.${track.album}`] ??= match.album;
	}
}

// ---------------------------------------------------------------- Navidrome

// Verified against Navidrome 0.62.0 (deluan/navidrome:0.62.0), which reports
// itself as "0.62.0 (1b46b977)":
// - The page at /app/ embeds the UI config, with "version" and "firstTime".
//   firstTime is true until the first user exists, so it is the fresh check.
// - POST /auth/createAdmin with {username, password} answers 200 with the
//   user's id, a JWT and a Subsonic salt and token. It was only ever sent to
//   a server with no users.
// - /rest/startScan and /rest/getScanStatus take u and p. The scan ended
//   with count 6 and folderCount 5, and search3 with an empty query then
//   listed everything.
// - ND_MUSICFOLDER, ND_ENABLEEXTERNALSERVICES=false and
//   ND_ENABLEINSIGHTSCOLLECTOR=false are the right names: the startup log
//   names the library path and says both are disabled. The server starts
//   with name lookups blocked.
// - All six tracks are accepted, short as they are. The two OK Computer
//   tracks land in two albums, because only the first carries a release id.
function navidromeAuth(row) {
	const common = [
		['v', '1.16.1'],
		['c', CLIENT],
		['f', 'json']
	];
	const auth = row.auth ?? 'default';
	const who =
		auth === 'none'
			? []
			: [
					['u', USER],
					['p', auth === 'wrong_password' ? 'wrong-password' : '{password}']
				];
	return { ...row, query: [...who, ...common, ...(row.query ?? [])] };
}

function rest(name, method, query = [], extra = {}) {
	return { name, path: `/rest/${method}`, query, ...extra };
}

function navidromeReads() {
	const artist = '{artist.radiohead}';
	const album = '{album.ok_computer}';
	const track = '{track.airbag}';
	const bytes = { read: 'bytes' };
	return [
		rest('ping', 'ping'),
		rest('getLicense', 'getLicense'),
		rest('getOpenSubsonicExtensions', 'getOpenSubsonicExtensions', [], { auth: 'none' }),
		rest('getMusicFolders', 'getMusicFolders'),
		rest('getUser', 'getUser', [['username', USER]]),
		rest('getScanStatus', 'getScanStatus'),
		rest('getArtists', 'getArtists'),
		rest('getIndexes', 'getIndexes'),
		rest('getArtist', 'getArtist', [['id', artist]]),
		rest('getAlbum', 'getAlbum', [['id', album]]),
		rest('getSong', 'getSong', [['id', track]]),
		rest('getMusicDirectory.artist', 'getMusicDirectory', [['id', artist]]),
		rest('getMusicDirectory.album', 'getMusicDirectory', [['id', album]]),
		rest('getGenres', 'getGenres'),
		rest('getSongsByGenre', 'getSongsByGenre', [
			['genre', 'Rock'],
			['count', '10']
		]),
		rest('getRandomSongs', 'getRandomSongs', [['size', '10']]),
		rest('getAlbumList2.newest', 'getAlbumList2', [['type', 'newest']]),
		rest('getAlbumList2.alphabeticalByName', 'getAlbumList2', [
			['type', 'alphabeticalByName'],
			['size', '2'],
			['offset', '1']
		]),
		rest('getAlbumList2.byYear', 'getAlbumList2', [
			['type', 'byYear'],
			['fromYear', '1990'],
			['toYear', '1999']
		]),
		rest('getAlbumList2.byGenre', 'getAlbumList2', [
			['type', 'byGenre'],
			['genre', 'Rock']
		]),
		rest('getAlbumList.newest', 'getAlbumList', [['type', 'newest']]),
		rest('search3.no-query', 'search3'),
		rest('search3.empty-quotes', 'search3', [['query', '""']]),
		rest('search3.word', 'search3', [
			['query', 'radiohead'],
			['artistCount', '1'],
			['albumCount', '1'],
			['songCount', '1'],
			['songOffset', '1']
		]),
		rest('search2.word', 'search2', [['query', 'radiohead']]),
		rest('getPlaylists', 'getPlaylists'),
		rest('getStarred2', 'getStarred2'),
		rest('getStarred', 'getStarred'),
		rest('getNowPlaying', 'getNowPlaying'),
		rest('getPlayQueue', 'getPlayQueue'),
		rest('getBookmarks', 'getBookmarks'),
		rest('getArtistInfo2', 'getArtistInfo2', [['id', artist]]),
		rest('getAlbumInfo2', 'getAlbumInfo2', [['id', album]]),
		rest('getTopSongs', 'getTopSongs', [['artist', 'Radiohead']]),
		rest('getSimilarSongs2', 'getSimilarSongs2', [['id', artist]]),
		rest('getLyricsBySongId', 'getLyricsBySongId', [['id', track]]),
		rest('getLyrics', 'getLyrics', [
			['artist', 'Radiohead'],
			['title', 'Airbag']
		]),
		rest('getCoverArt', 'getCoverArt', [['id', album]], bytes),
		rest('stream.full', 'stream', [['id', track]], bytes),
		rest('stream.range-first-50', 'stream', [['id', track]], {
			...bytes,
			headers: { range: 'bytes=0-49' }
		}),
		rest('stream.range-last-10', 'stream', [['id', track]], {
			...bytes,
			headers: { range: 'bytes=-10' }
		}),
		rest('stream.range-past-end', 'stream', [['id', track]], {
			...bytes,
			headers: { range: 'bytes=999999-' }
		}),
		rest('stream.head', 'stream', [['id', track]], { ...bytes, method: 'HEAD' }),
		rest(
			'stream.format-raw',
			'stream',
			[
				['id', track],
				['format', 'raw']
			],
			bytes
		),
		rest('download', 'download', [['id', track]], bytes),
		rest('error.wrong-password', 'ping', [], { auth: 'wrong_password' }),
		rest('error.missing-id', 'getAlbum'),
		rest('error.unknown-id', 'getAlbum', [['id', 'no-such-id']]),
		rest('error.unknown-method', 'noSuchMethod'),
		rest('error.stream-unknown-id', 'stream', [['id', 'no-such-id']], bytes),
		rest('error.music-folder-repeated', 'getArtists', [
			['musicFolderId', '1'],
			['musicFolderId', '1']
		]),
		rest('error.music-folder-unknown', 'getArtists', [['musicFolderId', '99']])
	];
}

function navidromeWrites() {
	const track = '{track.airbag}';
	const write = { write: true };
	return [
		rest('star', 'star', [['id', track]], write),
		rest(
			'createPlaylist',
			'createPlaylist',
			[
				['name', 'Reference'],
				['songId', track]
			],
			{
				...write,
				sets: { ref: 'playlist', at: ['subsonic-response', 'playlist', 'id'] }
			}
		),
		rest(
			'scrobble',
			'scrobble',
			[
				['id', track],
				['submission', 'true']
			],
			write
		),
		rest('getStarred2', 'getStarred2'),
		rest('getStarred', 'getStarred'),
		rest('getPlaylists', 'getPlaylists'),
		rest('getPlaylist', 'getPlaylist', [['id', '{playlist}']]),
		rest('getSong', 'getSong', [['id', track]]),
		rest('getAlbum', 'getAlbum', [['id', '{album.ok_computer}']]),
		rest('getNowPlaying', 'getNowPlaying'),
		rest('getAlbumList2.starred', 'getAlbumList2', [['type', 'starred']]),
		rest('getAlbumList2.recent', 'getAlbumList2', [['type', 'recent']]),
		rest('getAlbumList2.frequent', 'getAlbumList2', [['type', 'frequent']])
	];
}

// Arpeggi's artist browsing. Its library sync is the search3 below, copied
// from a request log in Arpeggi-App issue #6. It sends the folder id that
// getMusicFolders lists, which against gonic, numbering from 0, was 0
// (issue #153).
function navidromeArpeggi() {
	const sync = [
		['artistCount', '500'],
		['artistOffset', '0'],
		['albumCount', '0'],
		['albumOffset', '0'],
		['songCount', '0'],
		['songOffset', '0']
	];
	return [
		rest('getMusicFolders', 'getMusicFolders'),
		rest('search3.artist-sync', 'search3', [...sync, ['musicFolderId', '1']]),
		rest('getArtists.folder-1', 'getArtists', [['musicFolderId', '1']]),
		rest('getIndexes.if-modified-since', 'getIndexes', [
			['musicFolderId', '1'],
			['ifModifiedSince', '1760000000000']
		]),
		rest('getArtist.no-mbid', 'getArtist', [['id', '{artist.yuki}']]),
		rest('getArtists.folder-0', 'getArtists', [['musicFolderId', '0']]),
		rest('search3.folder-0', 'search3', [...sync, ['musicFolderId', '0']])
	];
}

// One rating each on a track, an album and an artist, then what shows them,
// then the track's rating cleared.
function navidromeRatings() {
	const track = '{track.airbag}';
	const album = '{album.ok_computer}';
	const artist = '{artist.radiohead}';
	const write = { write: true };
	const rate = (name, id, rating) =>
		rest(
			name,
			'setRating',
			[
				['id', id],
				['rating', rating]
			],
			write
		);
	return [
		rate('setRating.track', track, '4'),
		rate('setRating.album', album, '3'),
		rate('setRating.artist', artist, '5'),
		rest('getSong', 'getSong', [['id', track]]),
		rest('getAlbum', 'getAlbum', [['id', album]]),
		rest('getArtist', 'getArtist', [['id', artist]]),
		rest('getArtists', 'getArtists'),
		rest('search3', 'search3'),
		rest('getStarred2', 'getStarred2'),
		rest('getAlbumList2.highest', 'getAlbumList2', [['type', 'highest']]),
		rate('setRating.track-cleared', track, '0'),
		rest('getSong.cleared', 'getSong', [['id', track]])
	];
}

async function navidrome(refDir, args) {
	const { pinned, library, folders, set, capture, recorder } = prepare('navidrome', refDir, args);
	const { scrub, refs } = recorder;

	const page = await recorder.firstRead({ path: '/app/' });
	const config = (page.body?.text ?? '').replaceAll('\\', '');
	const version = /"version":"([^"]*)"/.exec(config)?.[1];
	const firstTime = /"firstTime":(true|false)/.exec(config)?.[1];
	if (!version || !firstTime) stop('the page at /app/ has no version or firstTime in its config');
	if (version !== pinned.version && !version.startsWith(`${pinned.version} `)) {
		stop(`this server says it is ${version}, not ${pinned.version}`);
	}
	if (firstTime !== 'true') stop('this server already has a user: only a fresh server is recorded');

	refs.password = crypto.randomBytes(12).toString('hex');
	scrub.secret(refs.password, '***');
	const admin = await recorder.ok({
		method: 'POST',
		path: '/auth/createAdmin',
		body: { username: USER, password: '{password}' }
	});
	scrub.secret(admin.body.token, '<token>');
	scrub.secret(admin.body.subsonicSalt, '<token>');
	scrub.secret(admin.body.subsonicToken, '<token>');
	scrub.id(admin.body.id);

	recorder.authorize = navidromeAuth;
	await recorder.ok(rest('', 'startScan'));
	let found = null;
	for (let attempt = 0; attempt < 60 && !found; attempt++) {
		await sleep(1000);
		const status = (await recorder.ok(rest('', 'getScanStatus'))).body['subsonic-response']
			.scanStatus;
		if (status.scanning) continue;
		const all = await recorder.ok(
			rest('', 'search3', [
				['query', ''],
				['artistCount', '500'],
				['albumCount', '500'],
				['songCount', '500']
			])
		);
		const result = all.body['subsonic-response'].searchResult3 ?? {};
		if ((result.song ?? []).length === library.tracks.length) found = result;
	}
	if (!found) stop(`the scan did not end with ${library.tracks.length} tracks`);
	console.log(`albums on the server: ${(found.album ?? []).map((a) => a.name).join(', ')}`);
	resolveByName(recorder, 'artist', library.artists, found.artist ?? []);
	resolveTracks(
		recorder,
		library,
		found.song.map((s) => ({ id: s.id, name: s.title, album: s.albumId }))
	);

	recorder.into(folders[0]);
	if (set === 'arpeggi') {
		await recorder.recordAll(navidromeArpeggi());
	} else if (set === 'ratings') {
		await recorder.recordAll(navidromeRatings());
	} else {
		await recorder.recordAll(navidromeReads());
		recorder.into(folders[1]);
		await recorder.recordAll(navidromeWrites());
	}
	recorder.write(capture(version));
}

// ---------------------------------------------------------------- Jellyfin

// Verified against Jellyfin 10.10.6 (jellyfin/jellyfin:10.10.6):
// - /System/Info/Public needs no credentials and carries Version and
//   StartupWizardCompleted, which is false until the wizard is done.
// - The wizard is POST /Startup/Configuration, GET then POST /Startup/User,
//   POST /Startup/RemoteAccess and POST /Startup/Complete. Each answers 2xx
//   with the client identity header and no token.
// - POST /Library/VirtualFolders takes name, collectionType, paths and
//   refreshLibrary in the query and LibraryOptions in the body. The list at
//   GET /Library/VirtualFolders shows the options only after a scan, and
//   the empty fetcher lists were kept.
// - POST /Library/Refresh starts the scan. It is over when /Items/Counts has
//   the six songs and the RefreshLibrary task in /ScheduledTasks is Idle.
// - The server starts with name lookups blocked. Its plugin manifest fetch
//   and one MusicBrainz artist lookup failed for that reason, so nothing
//   was fetched.
// - All six tracks are accepted, in five albums.
// - /Artists lists the credited artists only. Various Artists, the
//   compilation's album artist, is in /Artists/AlbumArtists.
// - With --set lyrics, the .lrc planted beside Airbag was picked up by the
//   first scan with the library options below as they are: no option was
//   added, and LrcLib stays disabled. Airbag then has HasLyrics true and a
//   second MediaStreams entry of Type Lyric, whose Path is the .lrc. A
//   track without lyrics answers /Audio/{id}/Lyrics with 404.
function jellyfinAuth(row) {
	const identity = `MediaBrowser Client="${CLIENT}", Device="${CLIENT}", DeviceId="${CLIENT}-device", Version="1.0"`;
	const auth = row.auth ?? 'default';
	if (auth === 'none') return row;
	if (auth === 'api_key') return { ...row, query: [...(row.query ?? []), ['api_key', '{token}']] };
	const header = auth === 'client' ? identity : `${identity}, Token="{token}"`;
	return { ...row, headers: { ...row.headers, authorization: header } };
}

function jf(name, path_, query = [], extra = {}) {
	return { name, path: path_, query, ...extra };
}

const MUSIC_TYPES = ['MusicArtist', 'MusicAlbum', 'Audio'];

function jellyfinLibraryOptions() {
	return {
		LibraryOptions: {
			Enabled: true,
			EnableRealtimeMonitor: false,
			EnableLUFSScan: false,
			EnableInternetProviders: false,
			SaveLocalMetadata: false,
			EnablePhotos: false,
			EnableChapterImageExtraction: false,
			ExtractChapterImagesDuringLibraryScan: false,
			AutomaticRefreshIntervalDays: 0,
			MetadataSavers: [],
			DisabledLyricFetchers: ['LrcLib'],
			PathInfos: [{ Path: '/media/music' }],
			TypeOptions: MUSIC_TYPES.map((Type) => ({
				Type,
				MetadataFetchers: [],
				MetadataFetcherOrder: [],
				ImageFetchers: [],
				ImageFetcherOrder: []
			}))
		}
	};
}

function jellyfinReads() {
	const user = ['userId', '{user}'];
	const recursive = ['Recursive', 'true'];
	const albums = ['IncludeItemTypes', 'MusicAlbum'];
	const album = '{album.ok_computer}';
	const track = '{track.airbag}';
	const artist = '{artist.radiohead}';
	const bytes = { read: 'bytes' };
	const direct = ['static', 'true'];
	const stream = `/Audio/${track}/stream`;
	return [
		jf('system.info', '/System/Info'),
		jf('quickconnect.enabled', '/QuickConnect/Enabled', [], { auth: 'none' }),
		jf('users.me', '/Users/Me'),
		jf('users.by-id', '/Users/{user}'),
		jf('userviews', '/UserViews', [user]),
		jf('userviews.legacy', '/Users/{user}/Views'),
		jf('items.albums', '/Items', [user, recursive, albums]),
		jf('items.tracks', '/Items', [user, recursive, ['IncludeItemTypes', 'Audio']]),
		jf('items.artists', '/Items', [user, recursive, ['IncludeItemTypes', 'MusicArtist']]),
		jf('items.playlists', '/Items', [user, recursive, ['IncludeItemTypes', 'Playlist']]),
		jf('items.genres', '/Items', [user, recursive, ['IncludeItemTypes', 'MusicGenre']]),
		jf('items.sort-premiere-date', '/Items', [user, recursive, albums, ['SortBy', 'PremiereDate']]),
		jf('items.sort-date-created-paged', '/Items', [
			user,
			recursive,
			albums,
			['SortBy', 'DateCreated'],
			['SortOrder', 'Descending'],
			['Limit', '2'],
			['StartIndex', '1']
		]),
		jf('items.parent-album', '/Items', [user, ['ParentId', album]]),
		jf('items.parent-library', '/Items', [user, ['ParentId', '{library}']]),
		jf('items.ids', '/Items', [user, ['Ids', `${track},{track.paranoid_android}`]]),
		jf('items.album-artist-ids', '/Items', [user, recursive, albums, ['AlbumArtistIds', artist]]),
		jf('items.filter-favorite', '/Items', [
			user,
			recursive,
			['IncludeItemTypes', 'Audio'],
			['Filters', 'IsFavorite']
		]),
		jf('items.no-total-count', '/Items', [
			user,
			recursive,
			albums,
			['EnableTotalRecordCount', 'false']
		]),
		jf('items.legacy', '/Users/{user}/Items', [recursive, albums]),
		jf('items.lowercase', '/items', [user, recursive, albums]),
		jf('latest', '/UserItems/Latest', [user, ['ParentId', '{library}']]),
		jf('latest.legacy', '/Users/{user}/Items/Latest', [['ParentId', '{library}']]),
		jf('artists', '/Artists', [user]),
		jf('artists.album-artists', '/Artists/AlbumArtists', [user]),
		jf('genres', '/Genres', [user]),
		jf('music-genres', '/MusicGenres', [user]),
		jf('items.filters', '/Items/Filters', [user, ['ParentId', '{library}']]),
		jf('item.album', `/Items/${album}`, [user]),
		jf('item.track', `/Items/${track}`, [user]),
		jf('item.artist', `/Items/${artist}`, [user]),
		jf('item.legacy', `/Users/{user}/Items/${track}`),
		jf('playback-info.get', `/Items/${track}/PlaybackInfo`, [user]),
		jf('playback-info.post', `/Items/${track}/PlaybackInfo`, [user], {
			method: 'POST',
			body: { UserId: '{user}', DeviceProfile: {} }
		}),
		jf('similar', `/Items/${track}/Similar`, [user]),
		jf('instant-mix.item', `/Items/${track}/InstantMix`, [user]),
		jf('instant-mix.artist', `/Artists/${artist}/InstantMix`, [user]),
		jf('image.primary', `/Items/${album}/Images/Primary`, [], { ...bytes, auth: 'none' }),
		jf('stream.full', stream, [direct], bytes),
		jf('stream.range-first-50', stream, [direct], { ...bytes, headers: { range: 'bytes=0-49' } }),
		jf('stream.range-last-10', stream, [direct], { ...bytes, headers: { range: 'bytes=-10' } }),
		jf('stream.range-past-end', stream, [direct], {
			...bytes,
			headers: { range: 'bytes=999999-' }
		}),
		jf('stream.head', stream, [direct], { ...bytes, method: 'HEAD' }),
		jf('stream.flac', `${stream}.flac`, [direct], bytes),
		jf(
			'universal.source',
			`/Audio/${track}/universal`,
			[user, ['Container', 'flac'], ['AudioCodec', 'flac']],
			{ read: 'headers' }
		),
		jf(
			'universal.mp3',
			`/Audio/${track}/universal`,
			[user, ['Container', 'mp3'], ['AudioCodec', 'mp3']],
			{ read: 'headers' }
		),
		jf('stream.no-credentials', stream, [direct], { ...bytes, auth: 'none' }),
		jf('stream.api-key', stream, [direct], { ...bytes, auth: 'api_key' }),
		jf('error.unknown-item', `/Items/${UNKNOWN_HEX_ID}`, [user]),
		jf('error.no-token', '/Items', [recursive, albums], { auth: 'none' }),
		jf('error.wrong-password', '/Users/AuthenticateByName', [], {
			method: 'POST',
			auth: 'client',
			body: { Username: USER, Pw: 'wrong-password' }
		}),
		jf('error.unknown-audio', `/Audio/${UNKNOWN_HEX_ID}/stream`, [direct], bytes)
	];
}

function jellyfinWrites() {
	const user = ['userId', '{user}'];
	const track = '{track.airbag}';
	const post = { method: 'POST', write: true };
	return [
		jf('favorite', `/UserFavoriteItems/${track}`, [user], post),
		jf('playlist.create', '/Playlists', [], {
			...post,
			body: { Name: 'Reference', Ids: [track], UserId: '{user}', MediaType: 'Audio' },
			sets: { ref: 'playlist', at: ['Id'] }
		}),
		jf('playing.stopped', '/Sessions/Playing/Stopped', [], {
			...post,
			body: { ItemId: track, PositionTicks: 10000000 }
		}),
		jf('items.filter-favorite', '/Items', [
			user,
			['Recursive', 'true'],
			['IncludeItemTypes', 'Audio'],
			['Filters', 'IsFavorite']
		]),
		jf('items.filter-played', '/Items', [
			user,
			['Recursive', 'true'],
			['IncludeItemTypes', 'Audio'],
			['Filters', 'IsPlayed']
		]),
		jf('items.sort-date-played', '/Items', [
			user,
			['Recursive', 'true'],
			['IncludeItemTypes', 'Audio'],
			['SortBy', 'DatePlayed'],
			['SortOrder', 'Descending']
		]),
		jf('items.playlists', '/Items', [
			user,
			['Recursive', 'true'],
			['IncludeItemTypes', 'Playlist']
		]),
		jf('item.track', `/Items/${track}`, [user]),
		jf('playlist', '/Playlists/{playlist}'),
		jf('playlist.items', '/Playlists/{playlist}/Items', [user])
	];
}

// Lyrics, with lyrics_airbag.lrc planted beside Airbag: the two tracks
// alone, the album's tracks, PlaybackInfo as Finamp sends it, then the
// lyrics address for both, an unknown id and no token.
function jellyfinLyrics() {
	const user = ['userId', '{user}'];
	const playbackInfo = (name, track) =>
		jf(name, `/Items/${track}/PlaybackInfo`, [user], {
			method: 'POST',
			body: { UserId: '{user}', DeviceProfile: {} }
		});
	return [
		jf('item.airbag', '/Items/{track.airbag}', [user]),
		jf('item.paranoid-android', '/Items/{track.paranoid_android}', [user]),
		jf('items.parent-album', '/Items', [user, ['ParentId', '{album.ok_computer}']]),
		playbackInfo('playback-info.airbag', '{track.airbag}'),
		playbackInfo('playback-info.paranoid-android', '{track.paranoid_android}'),
		jf('lyrics.airbag', '/Audio/{track.airbag}/Lyrics'),
		jf('lyrics.paranoid-android', '/Audio/{track.paranoid_android}/Lyrics'),
		jf('error.lyrics-unknown', `/Audio/${UNKNOWN_HEX_ID}/Lyrics`),
		jf('error.lyrics-no-token', '/Audio/{track.airbag}/Lyrics', [], { auth: 'none' })
	];
}

// Played marks: a track marked on the legacy path, its album marked on the
// modern one, the played filter and the DatePlayed sort, a mark on each of
// two artists with their tracks by credit and by album artist, both marks
// taken off again, then an unknown id.
function jellyfinPlayed() {
	const user = ['userId', '{user}'];
	const recursive = ['Recursive', 'true'];
	const tracks = ['IncludeItemTypes', 'Audio'];
	const albums = ['IncludeItemTypes', 'MusicAlbum'];
	const post = { method: 'POST', write: true };
	const remove = { method: 'DELETE', write: true };
	const track = '{track.airbag}';
	const album = '{album.ok_computer}';
	const artist = (key) => {
		const id = `{artist.${key}}`;
		return [
			jf(`played.artist.${key}`, `/UserPlayedItems/${id}`, [user], post),
			jf(`item.artist.${key}`, `/Items/${id}`, [user]),
			jf(`items.artist-ids.${key}`, '/Items', [user, recursive, tracks, ['ArtistIds', id]]),
			jf(`items.album-artist-ids.${key}`, '/Items', [
				user,
				recursive,
				tracks,
				['AlbumArtistIds', id]
			])
		];
	};
	return [
		jf('played.track.legacy', `/Users/{user}/PlayedItems/${track}`, [], post),
		jf('item.track.played', `/Items/${track}`, [user]),
		jf('item.album.one-track-played', `/Items/${album}`, [user]),
		jf('played.album', `/UserPlayedItems/${album}`, [user], post),
		jf('item.album.played', `/Items/${album}`, [user]),
		jf('items.parent-album.played', '/Items', [user, ['ParentId', album]]),
		jf('items.albums.filter-played', '/Items', [user, recursive, albums, ['Filters', 'IsPlayed']]),
		jf('items.albums.filter-unplayed', '/Items', [
			user,
			recursive,
			albums,
			['Filters', 'IsUnplayed']
		]),
		jf('items.tracks.sort-date-played', '/Items', [
			user,
			recursive,
			tracks,
			['SortBy', 'DatePlayed'],
			['SortOrder', 'Descending']
		]),
		...artist('massive_attack'),
		...artist('various_artists'),
		jf('unplayed.track', `/UserPlayedItems/${track}`, [user], remove),
		jf('unplayed.album.legacy', `/Users/{user}/PlayedItems/${album}`, [], remove),
		jf('item.track.unplayed', `/Items/${track}`, [user]),
		jf('item.album.unplayed', `/Items/${album}`, [user]),
		jf('items.tracks.filter-played', '/Items', [user, recursive, tracks, ['Filters', 'IsPlayed']]),
		jf('error.played-unknown', `/UserPlayedItems/${UNKNOWN_HEX_ID}`, [user], post)
	];
}

// Paging without SortBy: pages that start off a multiple of Limit, with
// and without the total, for albums and tracks.
function jellyfinPaging() {
	const user = ['userId', '{user}'];
	const recursive = ['Recursive', 'true'];
	const albums = ['IncludeItemTypes', 'MusicAlbum'];
	const tracks = ['IncludeItemTypes', 'Audio'];
	const noTotal = ['EnableTotalRecordCount', 'false'];
	const page = (start) => [
		['StartIndex', start],
		['Limit', '2']
	];
	return [
		jf('items.albums.paged', '/Items', [user, recursive, albums, ...page('3')]),
		jf('items.albums.paged-no-total', '/Items', [user, recursive, albums, ...page('1'), noTotal]),
		jf('items.tracks.paged-no-total', '/Items', [user, recursive, tracks, ...page('1'), noTotal])
	];
}

async function jellyfin(refDir, args) {
	const { pinned, library, folders, set, capture, recorder } = prepare('jellyfin', refDir, args);
	const { scrub, refs } = recorder;
	const publicInfo = jf('system.info.public', '/System/Info/Public', [], { auth: 'none' });

	const info = (await recorder.firstRead(publicInfo)).body;
	if (info.Version !== pinned.version)
		stop(`this server says it is ${info.Version}, not ${pinned.version}`);
	if (info.StartupWizardCompleted !== false)
		stop('this server has finished its setup wizard: only a fresh server is recorded');

	refs.password = crypto.randomBytes(12).toString('hex');
	refs.user_name = USER;
	scrub.secret(refs.password, '***');
	recorder.authorize = jellyfinAuth;
	const setup = { method: 'POST', auth: 'client' };
	await recorder.ok({
		...setup,
		path: '/Startup/Configuration',
		body: { UICulture: 'en-US', MetadataCountryCode: 'US', PreferredMetadataLanguage: 'en' }
	});
	await recorder.ok({ auth: 'client', path: '/Startup/User' });
	await recorder.ok({
		...setup,
		path: '/Startup/User',
		body: { Name: USER, Password: '{password}' }
	});
	await recorder.ok({
		...setup,
		path: '/Startup/RemoteAccess',
		body: { EnableRemoteAccess: true, EnableAutomaticPortMapping: false }
	});
	await recorder.ok({ ...setup, path: '/Startup/Complete' });

	recorder.into(folders[0]);
	await recorder.record(publicInfo);
	const login = await recorder.record(
		jf('login', '/Users/AuthenticateByName', [], {
			method: 'POST',
			auth: 'client',
			body: { Username: '{user_name}', Pw: '{password}' }
		})
	);
	if (login.status !== 200) stop(`login answered ${login.status}`);
	refs.token = login.body.AccessToken;
	refs.user = login.body.User.Id;
	scrub.secret(refs.token, '<token>');

	await recorder.ok({
		method: 'POST',
		path: '/Library/VirtualFolders',
		query: [
			['name', 'Music'],
			['collectionType', 'music'],
			['paths', '/media/music'],
			['refreshLibrary', 'false']
		],
		body: jellyfinLibraryOptions()
	});
	await recorder.ok({ method: 'POST', path: '/Library/Refresh' });
	let settled = 0;
	for (let attempt = 0; attempt < 120 && settled < 2; attempt++) {
		await sleep(1000);
		const counts = (await recorder.ok({ path: '/Items/Counts', query: [['userId', '{user}']] }))
			.body;
		const tasks = (await recorder.ok({ path: '/ScheduledTasks' })).body;
		const idle = tasks.find((task) => task.Key === 'RefreshLibrary')?.State === 'Idle';
		settled = idle && counts.SongCount === library.tracks.length ? settled + 1 : 0;
	}
	if (settled < 2) stop(`the scan did not end with ${library.tracks.length} tracks`);

	// The list carries a library's options only once a scan has made its item.
	const listed = (await recorder.ok({ path: '/Library/VirtualFolders' })).body;
	const typeOptions = listed?.[0]?.LibraryOptions?.TypeOptions;
	if (!Array.isArray(typeOptions))
		stop(`unexpected library list: ${JSON.stringify(listed).slice(0, 3000)}`);
	for (const type of typeOptions) {
		if (type.MetadataFetchers.length || type.ImageFetchers.length)
			stop(`the library still has fetchers for ${type.Type}`);
	}
	const views = (await recorder.ok({ path: '/UserViews', query: [['userId', '{user}']] })).body
		.Items;
	const music = views.find((view) => view.CollectionType === 'music');
	if (!music) stop('no music view');
	refs.library = music.Id;
	const named = (answer) =>
		answer.body.Items.map((item) => ({
			id: item.Id,
			name: item.Name,
			type: item.Type,
			album: item.AlbumId
		}));
	const items = named(
		await recorder.ok({
			path: '/Items',
			query: [
				['userId', '{user}'],
				['Recursive', 'true'],
				['IncludeItemTypes', 'MusicAlbum,Audio']
			]
		})
	);
	const artists = named(await recorder.ok({ path: '/Artists', query: [['userId', '{user}']] }));
	const albumArtists = named(
		await recorder.ok({ path: '/Artists/AlbumArtists', query: [['userId', '{user}']] })
	);
	for (const albumArtist of albumArtists)
		if (!artists.some((known) => known.id === albumArtist.id)) artists.push(albumArtist);
	resolveByName(recorder, 'artist', library.artists, artists);
	console.log(
		`albums on the server: ${items
			.filter((item) => item.type === 'MusicAlbum')
			.map((a) => a.name)
			.join(', ')}`
	);
	resolveTracks(
		recorder,
		library,
		items.filter((item) => item.type === 'Audio')
	);

	if (set === 'lyrics') {
		await recorder.recordAll(jellyfinLyrics());
	} else if (set === 'played') {
		await recorder.recordAll(jellyfinPlayed());
	} else if (set === 'paging') {
		await recorder.recordAll(jellyfinPaging());
	} else {
		await recorder.recordAll(jellyfinReads());
		recorder.into(folders[1]);
		await recorder.recordAll(jellyfinWrites());
	}
	recorder.write(capture(info.Version));
}

// ---------------------------------------------------------------- check

function tokenProblems(value, found = []) {
	if (Array.isArray(value)) value.forEach((item) => tokenProblems(item, found));
	else if (value && typeof value === 'object') {
		for (const [key, inner] of Object.entries(value)) {
			if (
				typeof inner === 'string' &&
				TOKEN_KEY.test(key) &&
				!['<token>', '{password}', 'wrong-password'].includes(inner)
			)
				found.push(key);
			tokenProblems(inner, found);
		}
	}
	return found;
}

function listFiles(dir) {
	return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
		const full = path.join(dir, entry.name);
		return entry.isDirectory() ? listFiles(full) : [full];
	});
}

function check(refDir) {
	const root = path.resolve(refDir);
	const problems = [];
	const library = loadLibrary(root);
	for (const file of listFiles(root)) {
		const name = path.relative(root, file);
		const text = fs.readFileSync(file, 'utf8');
		for (const [what, pattern] of FORBIDDEN)
			if (pattern.test(text)) problems.push(`${name}: holds ${what}`);
		if (/[?&]p=(?!\*\*\*|wrong-password)/.test(text)) problems.push(`${name}: holds a password`);
		if (
			/(api_key|apikey)=(?!<token>|\{token\})/i.test(text) ||
			/Token=\\?"(?!<token>|\{token\})/.test(text)
		)
			problems.push(`${name}: holds a token`);
		if (!name.endsWith('.json')) continue;
		try {
			for (const key of tokenProblems(JSON.parse(text)))
				problems.push(`${name}: ${key} is not scrubbed`);
		} catch {
			problems.push(`${name}: is not JSON`);
		}
	}
	const folders = fs
		.readdirSync(root, { withFileTypes: true })
		.filter((entry) => entry.isDirectory());
	if (folders.length === 0) problems.push('no capture folders');
	let files = 0;
	for (const { name: folder } of folders) {
		const dir = path.join(root, folder);
		const listed = new Map(
			fs
				.readFileSync(path.join(dir, 'SHA256SUMS'), 'utf8')
				.split('\n')
				.filter(Boolean)
				.map((line) => [line.slice(66), line.slice(0, 64)])
		);
		const present = fs.readdirSync(dir).filter((name) => name !== 'SHA256SUMS');
		for (const name of present) {
			if (!listed.has(name)) problems.push(`${folder}/${name}: not in SHA256SUMS`);
			else if (sha256(fs.readFileSync(path.join(dir, name))) !== listed.get(name))
				problems.push(`${folder}/${name}: changed since it was recorded`);
		}
		for (const name of listed.keys())
			if (!present.includes(name)) problems.push(`${folder}/${name}: missing`);
		const capture = JSON.parse(fs.readFileSync(path.join(dir, 'capture.json'), 'utf8'));
		if (capture.library_sha256 !== library.sha256)
			problems.push(`${folder}: library.json changed since it was recorded`);
		const pinned = SERVERS[capture.server];
		if (
			!pinned ||
			capture.image !== pinned.image ||
			!/^sha256:[0-9a-f]{64}$/.test(capture.image_id) ||
			!capture.reported_version
		) {
			problems.push(
				`${folder}: capture.json does not name the pinned image, its id and the reported version`
			);
		}
		const index = JSON.parse(fs.readFileSync(path.join(dir, 'index.json'), 'utf8'));
		for (const row of index.requests)
			if (!listed.has(row.file))
				problems.push(`${folder}: index.json names ${row.file}, which is missing`);
		files += present.length;
	}
	if (problems.length) stop(`check failed:\n  ${problems.join('\n  ')}`);
	console.log(
		`ok: ${files} files in ${folders.length} folders match their checksums and hold nothing private`
	);
}

// ---------------------------------------------------------------- main

function parseArgs(argv) {
	const positional = [];
	const named = {};
	for (let i = 0; i < argv.length; i++) {
		if (argv[i].startsWith('--')) named[argv[i].slice(2)] = argv[++i];
		else positional.push(argv[i]);
	}
	return { positional, named };
}

async function main() {
	const [command, ...rest_] = process.argv.slice(2);
	const { positional, named } = parseArgs(rest_);
	const need = (count) => {
		if (positional.length !== count) stop('usage: see the top of this script');
	};
	if (command === 'plant') return (need(2), plant(positional[0], positional[1], named));
	if (command === 'clean') return (need(1), clean(positional[0]));
	if (command === 'check') return (need(1), check(positional[0]));
	if (command === 'navidrome') return (need(1), navidrome(positional[0], named));
	if (command === 'jellyfin') return (need(1), jellyfin(positional[0], named));
	stop('usage: see the top of this script');
}

main().catch((error) => {
	console.error(error instanceof Stop ? `stopped: ${error.message}` : error);
	process.exit(1);
});
