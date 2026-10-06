import prettier from 'eslint-config-prettier';
import { fileURLToPath } from 'node:url';
import { includeIgnoreFile } from '@eslint/compat';
import js from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import { defineConfig } from 'eslint/config';
import globals from 'globals';
import ts from 'typescript-eslint';
import svelteConfig from './svelte.config.js';

const gitignorePath = fileURLToPath(new URL('./.gitignore', import.meta.url));

// Writes to the query cache must go through the persister helpers.
const queryClientRules = [
	// Ensure not to call queryClient.setQueriesData or queryClient.setQueryData directly, as this will bypass the persister and lead to data loss on page refresh.
	{
		selector:
			"CallExpression[callee.object.name='queryClient'][callee.property.name='setQueriesData']",
		message:
			"Direct use of 'queryClient.setQueriesData' is forbidden. Please use 'setQueryDataWithPersister' per matching query key instead (no plural helper exists)."
	},
	{
		selector:
			"CallExpression[callee.object.name='queryClient'][callee.property.name='setQueryData']",
		message:
			"Direct use of 'queryClient.setQueryData' is forbidden. Please use the 'setQueryDataWithPersister' function instead."
	},
	{
		selector:
			"CallExpression[callee.object.name='queryClient'][callee.property.name='invalidateQueries']",
		message:
			"Direct use of 'queryClient.invalidateQueries' is forbidden. Please use the custom 'invalidateQueriesWithPersister' hook instead."
	}
];

// Server calls go through the transport in src/lib/api/: URLs come from v3()
// registry templates, requests from the api client. An API path literal or a
// raw fetch anywhere else skips the contract check, so both are errors outside
// the transport. A path literal is allowed only as the template argument of
// v3() and as a type argument naming a contract path. Module specifiers such
// as '$lib/api/client' are not paths and do not match; a full URL to a
// versioned API path ('https://host/api/v3/...') does, and so does a bare
// base such as '/api/v1' with nothing after it, while third-party links such
// as 'https://www.last.fm/api/account' do not. Event streams, beacons and XHR
// are server channels too, so they stay in the transport as well.
const API_PATH = '/(^|[^\\w$.]|:\\/\\/[^/]*)\\/api\\/v\\d+(?=\\/|$)/';
const transportRules = [
	{
		selector: `Literal[value=${API_PATH}]:not(CallExpression[callee.name='v3'] > Literal.arguments):not(TSLiteralType > Literal)`,
		message:
			'API paths belong in a v3() registry template, or on the waiting-on-backend list in eslint.config.js.'
	},
	{
		selector: `TemplateElement[value.raw=${API_PATH}]`,
		message:
			'API paths belong in a v3() registry template, or on the waiting-on-backend list in eslint.config.js.'
	},
	{
		// static attribute values in Svelte markup (href="/api/...")
		selector: `SvelteLiteral[value=${API_PATH}]`,
		message:
			'API paths belong in a v3() registry template, or on the waiting-on-backend list in eslint.config.js.'
	},
	{
		// fetch by any route: called, aliased, destructured or passed along
		selector:
			"Identifier[name='fetch']:not(MemberExpression[computed=false] > Identifier.property):not(Property > Identifier.key):not(TSPropertySignature > Identifier.key):not(MethodDefinition > Identifier.key)",
		message: 'Use the api client from $lib/api/client instead of fetch.'
	},
	{
		// window.fetch, globalThis.fetch, a load event's event.fetch
		selector: "MemberExpression[computed=false][property.name='fetch']",
		message: 'Use the api client from $lib/api/client instead of fetch.'
	},
	{
		selector: 'NewExpression[callee.name=/^(EventSource|XMLHttpRequest)$/]',
		message: 'Open server channels through $lib/api (openEventStream) instead.'
	},
	{
		selector: "MemberExpression[computed=false][property.name='sendBeacon']",
		message: 'Send beacons through $lib/api/channels (sendJsonBeacon) instead.'
	}
];

// The one exception list for the transport rules: the calls the v3 server has
// no route for yet. Their URL builders all live in lib/constants.ts (the API
// registry), grouped by feature: membership and album status, edition
// conversions, scan diagnostics, reviews, bulk review and management
// operations, repairs and identity preparations, Library Management previews
// and recovery, cache sync, YouTube, local file downloads, Free Music, drop
// import, the download queue, held imports and upgrades. When a route lands,
// its call moves onto a v3() template and its builder leaves that file.
const WAITING_ON_BACKEND = ['src/lib/constants.ts'];

// The transport itself: the api client and the page-scoped fetch it wraps.
const TRANSPORT = ['src/lib/api/**', 'src/lib/utils/navigationAbort.ts'];

export default defineConfig(
	includeIgnoreFile(gitignorePath),
	js.configs.recommended,
	...ts.configs.recommended,
	...svelte.configs.recommended,
	prettier,
	...svelte.configs.prettier,
	{
		languageOptions: {
			globals: { ...globals.browser, ...globals.node }
		},
		rules: {
			// typescript-eslint strongly recommend that you do not use the no-undef lint rule on TypeScript projects.
			// see: https://typescript-eslint.io/troubleshooting/faqs/eslint/#i-get-errors-from-the-no-undef-rule-about-global-variables-not-being-defined-even-though-there-are-no-typescript-errors
			'no-undef': 'off',
			'@typescript-eslint/no-explicit-any': 'error',
			'@typescript-eslint/no-unused-vars': [
				'error',
				{ argsIgnorePattern: '^_', varsIgnorePattern: '^_', caughtErrorsIgnorePattern: '^_' }
			]
		}
	},
	{
		files: ['**/*.svelte', '**/*.svelte.ts', '**/*.svelte.js'],
		languageOptions: {
			parserOptions: {
				projectService: true,
				extraFileExtensions: ['.svelte'],
				parser: ts.parser,
				svelteConfig
			}
		},
		rules: {
			'svelte/no-navigation-without-resolve': 'off'
		}
	},
	{
		rules: {
			'no-restricted-syntax': ['error', ...queryClientRules]
		}
	},
	{
		files: ['src/**/*.ts', 'src/**/*.js', 'src/**/*.svelte'],
		ignores: [
			...TRANSPORT,
			...WAITING_ON_BACKEND,
			'src/**/*.spec.ts',
			'src/**/*.test.ts',
			'src/**/*.d.ts'
		],
		rules: {
			'no-restricted-syntax': ['error', ...queryClientRules, ...transportRules]
		}
	}
);
