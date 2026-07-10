<script lang="ts">
	import { Github } from 'lucide-svelte';
	import { getVersionQuery } from '$lib/queries/VersionQuery.svelte';

	const GITHUB_URL = 'https://github.com/DroppedNeedle/DroppedNeedle';

	const versionQuery = getVersionQuery();
	const version = $derived(versionQuery.data?.version ?? null);
	const versionLabel = $derived(
		version === null
			? null
			: version === 'dev' || version === 'hosting-local'
				? 'Host Local'
				: `v${version}`
	);
</script>

<footer class="dn-footer" aria-label="Site footer">
	<div class="dn-footer__inner">
		<span class="dn-footer__wordmark">DroppedNeedle</span>
		<span class="dn-footer__tag">Audiophile grade · Your music, on the record</span>
		<div class="dn-footer__links">
			<a class="dn-footer__link" href={GITHUB_URL} target="_blank" rel="noopener noreferrer">
				<Github class="h-3.5 w-3.5" />
				<span>GitHub</span>
			</a>
			{#if versionLabel}
				<span class="dn-footer__ver">{versionLabel}</span>
			{/if}
		</div>
	</div>
</footer>

<style>
	.dn-footer {
		margin-top: auto;
		padding: 1.5rem 0 1.75rem;
		border-top: 1px solid var(--dn-hairline);
	}

	.dn-footer__inner {
		width: 100%;
		max-width: 80rem;
		margin: 0 auto;
		padding-inline: clamp(1rem, 4vw, 2.5rem);
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0.5rem 1.5rem;
	}

	.dn-footer__wordmark {
		font-family: var(--font-display);
		font-size: 0.95rem;
		font-weight: 700;
		color: oklch(from var(--color-base-content) l c h / 0.55);
	}

	.dn-footer__tag {
		flex: 1;
		font-family: var(--font-mono);
		font-size: 0.6rem;
		font-weight: 700;
		letter-spacing: 0.18em;
		text-transform: uppercase;
		white-space: nowrap;
		color: oklch(from var(--color-base-content) l c h / 0.3);
	}

	.dn-footer__links {
		display: flex;
		align-items: center;
		gap: 1.25rem;
		font-family: var(--font-mono);
		font-size: 0.68rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	.dn-footer__link {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		color: oklch(from var(--color-base-content) l c h / 0.6);
		transition: color 0.15s ease;
	}
	.dn-footer__link:hover {
		color: oklch(from var(--color-base-content) l c h);
	}
	.dn-footer__link:focus-visible {
		outline: 2px solid oklch(from var(--color-primary) l c h / 0.6);
		outline-offset: 3px;
		border-radius: 4px;
	}

	.dn-footer__ver {
		color: oklch(from var(--color-base-content) l c h / 0.35);
	}
</style>
