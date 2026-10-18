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

// The page's content-security policy (server/src/security_headers.rs) refuses
// a style attribute written into markup, so a static style="..." on an element
// is refused in every browser while vite dev and the specs, which send no
// policy, show nothing. A style attribute with an expression in it, style={x}
// and style: directives are set through the style object, which the policy
// does not govern, and stay allowed. eslint-plugin-svelte 3.23.0's
// svelte/no-inline-styles cannot tell the two apart (its one option is
// allowTransitions), hence a selector.
const markupRules = [
	{
		selector:
			"SvelteElement[kind='html'] > SvelteStartTag > SvelteAttribute[key.name='style'][value.length=1][value.0.type='SvelteLiteral']",
		message:
			'The content-security policy refuses a static style attribute. Use a class, or style: directives.'
	}
];

// Buttons, cards and text inputs come from the kit in src/lib/ui, so their
// look is defined in one place. Outside the kit a DaisyUI btn or card class
// (bare or with one of its modifiers) is refused wherever a class can come
// from: a class attribute, a class: directive, or a string or template
// literal. Only whole class tokens match, optionally behind variant prefixes
// such as md:, so names like crate-card, tab-btn or group/card do not.
const BTN_CLASS =
	'btn(-(xs|sm|md|lg|xl|primary|secondary|accent|neutral|ghost|outline|link|error|warning|success|info|circle|square|wide|block|active|soft|dash))?';
const CARD_CLASS = 'card(-(body|title|actions|xs|sm|md|lg|xl|side|border|dash))?';
const kitClassRules = (name, message) => {
	const token = `/(^|\\s)([^\\s:]+:)*${name}(\\s|$)/`;
	return [
		{ selector: `SvelteAttribute[key.name='class'] > SvelteLiteral[value=${token}]`, message },
		{ selector: `SvelteDirective[kind='Class'][key.name.name=/^${name}$/]`, message },
		{ selector: `Literal[value=${token}]`, message },
		{ selector: `TemplateElement[value.raw=${token}]`, message }
	];
};
// Checkboxes, radios, ranges, file inputs, selects and textareas stay plain.
const TEXT_INPUT_TYPE = '/^(text|search|url|email|password|number|date|time|tel)$/';
const TEXT_INPUT_MESSAGE =
	'Text inputs come from the kit: use Input, or TextField, NumberField or SecretField for a labelled row, from $lib/ui.';
const INPUT_TAG = "SvelteElement[kind='html'][name.name='input'] > SvelteStartTag";
const kitRules = [
	...kitClassRules(
		BTN_CLASS,
		'Buttons come from the kit: use Button, IconButton or AsyncButton from $lib/ui.'
	),
	...kitClassRules(
		CARD_CLASS,
		'Cards come from the kit: use Card from $lib/ui, with its title, header and actions.'
	),
	{
		selector: `${INPUT_TAG}:not(:has(SvelteAttribute[key.name='type']))`,
		message: TEXT_INPUT_MESSAGE
	},
	{
		// type="text", or type={...} that can evaluate to a text-like type
		selector: `${INPUT_TAG} > SvelteAttribute[key.name='type'] :matches(SvelteLiteral, Literal)[value=${TEXT_INPUT_TYPE}]`,
		message: TEXT_INPUT_MESSAGE
	}
];

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
			'no-restricted-syntax': ['error', ...queryClientRules, ...markupRules]
		}
	},
	{
		// The one component allowed to render a string as markup: it sanitizes
		// first. Everywhere else svelte/no-at-html-tags stays an error.
		files: ['src/lib/components/SafeHtml.svelte'],
		rules: {
			'svelte/no-at-html-tags': 'off'
		}
	},
	{
		files: ['src/**/*.ts', 'src/**/*.js', 'src/**/*.svelte'],
		ignores: [...TRANSPORT, 'src/**/*.spec.ts', 'src/**/*.test.ts', 'src/**/*.d.ts'],
		rules: {
			'no-restricted-syntax': ['error', ...queryClientRules, ...transportRules, ...markupRules]
		}
	},
	{
		files: ['src/**/*.svelte'],
		ignores: [...TRANSPORT, 'src/lib/ui/**'],
		rules: {
			'no-restricted-syntax': [
				'error',
				...queryClientRules,
				...transportRules,
				...markupRules,
				...kitRules
			]
		}
	}
);
