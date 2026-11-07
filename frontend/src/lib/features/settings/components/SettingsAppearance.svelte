<!--
	The site's CSS, under every person's own, and the switch that lets people
	add their own. The editor marks what the check refuses as you type; a save
	the server refuses marks the lines the server named.
-->
<script lang="ts">
	import {
		createSaveSiteAppearanceMutation,
		getSiteAppearanceQuery,
		type SiteAppearanceSettings
	} from '$lib/features/settings/queries/appearance.svelte';
	import CssSnippetsMenu from '$lib/features/profile/components/CssSnippetsMenu.svelte';
	import CssVariablesPanel from '$lib/features/profile/components/CssVariablesPanel.svelte';
	import { CSS_PLACEHOLDER, CSS_WORDS } from '$lib/features/profile/components/cssVocabulary';
	import '$lib/features/profile/components/customCss.css';
	import { ApiError } from '$lib/api/errors';
	import type { components } from '$lib/api/v3/openapi';
	import {
		customCssProblems,
		customCssSaveError,
		describeCssProblems,
		MAX_SITE_CSS
	} from '$lib/stores/customCss.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CodeField from '$lib/ui/CodeField.svelte';
	import type { CodeProblem } from '$lib/ui/codeEditor';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';
	import Tabs from '$lib/ui/Tabs.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import SettingsPaneHeader from './SettingsPaneHeader.svelte';

	type CssProblem = components['schemas']['CssProblem'];
	type RailTab = 'variables' | 'snippets';

	interface Refusal {
		text: string;
		problems: CssProblem[];
	}

	const saveSite = createSaveSiteAppearanceMutation();
	let refusal = $state.raw<Refusal | null>(null);

	const section = createSettingsSection<SiteAppearanceSettings>(
		getSiteAppearanceQuery(),
		{
			mutateAsync: async (settings) => {
				try {
					const saved = await saveSite.mutateAsync(settings);
					refusal = null;
					return saved;
				} catch (error) {
					refusal = { text: settings.site_css, problems: refusedProblems(error) };
					throw error;
				}
			},
			get isPending() {
				return saveSite.isPending;
			}
		},
		{ explain: (error) => customCssSaveError(error, 'site') }
	);

	let field = $state<CodeField | null>(null);
	let rail = $state<RailTab>('variables');
	const tabs = [
		{ id: 'variables' as const, label: 'Variables' },
		{ id: 'snippets' as const, label: 'Snippets' }
	];

	const text = $derived(section.draft?.site_css ?? '');
	const typedProblems = $derived(customCssProblems(text, 'site'));

	function refusedProblems(error: unknown): CssProblem[] {
		if (!(error instanceof ApiError) || error.code !== 'CUSTOM_CSS_REFUSED') return [];
		const details = error.details as { problems?: unknown } | null;
		return Array.isArray(details?.problems) ? (details.problems as CssProblem[]) : [];
	}

	// While the text is the one the server refused, its answer marks the lines.
	const markers = $derived.by(() => {
		const refused = refusal;
		return (current: string): CodeProblem[] => {
			const found =
				refused && current === refused.text && refused.problems.length > 0
					? refused.problems
					: customCssProblems(current, 'site');
			return found.map((problem) => ({
				line: problem.line,
				message: describeCssProblems([problem], 'site') ?? problem.what
			}));
		};
	});
</script>

<div class="space-y-5">
	<SettingsPaneHeader
		title="Appearance"
		description="Site CSS applies to everyone, under each person's own CSS."
	/>
	<Card variant="bordered" as="section">
		<QueryBoundary query={section} what="the site appearance">
			{#snippet children(draft)}
				<div class="space-y-5">
					<ToggleRow
						label="Let people add their own CSS"
						description="Off hides everyone's own CSS and keeps their text."
						bind:checked={draft.allow_user_css}
						disabled={section.saving}
					/>
					<div class="site-css-root">
						<div class="site-css">
							<div class="site-css__editor">
								<CodeField
									bind:this={field}
									bind:value={draft.site_css}
									label="Site CSS"
									limit={MAX_SITE_CSS}
									words={[...CSS_WORDS]}
									problems={markers}
									placeholder={CSS_PLACEHOLDER}
									error={describeCssProblems(typedProblems, 'site')}
									help="Add ?nocustomcss=1 to the address to turn it off."
								/>
							</div>
							<div class="site-css__rail">
								<Tabs
									{tabs}
									selected={rail}
									onselect={(id) => (rail = id)}
									label="Editor helpers"
									idPrefix="site-css-rail"
									look="border"
									panelClass="css-rail-panel"
								>
									{#if rail === 'variables'}
										<CssVariablesPanel oninsert={(css) => field?.insert(css)} />
									{:else}
										<CssSnippetsMenu oninsert={(css) => field?.insert(css)} />
									{/if}
								</Tabs>
							</div>
						</div>
					</div>
					<SaveBar {...section.bar} disabled={typedProblems.length > 0} />
				</div>
			{/snippet}
		</QueryBoundary>
	</Card>
</div>

<style>
	.site-css-root {
		container-type: inline-size;
		/* Shorter than the profile's list, so the Save bar fits a 900px-tall desktop screen. */
		--css-rail-list-max: 16rem;
	}
	.site-css {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 1rem;
	}
	.site-css__editor,
	.site-css__rail {
		min-width: 0;
	}
	@container (min-width: 40rem) {
		.site-css {
			grid-template-columns: minmax(0, 1fr) 16rem;
			align-items: start;
		}
	}
</style>
