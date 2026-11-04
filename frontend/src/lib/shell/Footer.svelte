<!--
	The page's foot: the frit rising from the bottom edge, then a plain bar
	with the logo, the tagline, the GitHub link and the version.
-->
<script lang="ts">
	import GitHubIcon from '$lib/media/icons/GitHubIcon.svelte';
	import FritEdge from '$lib/ui/FritEdge.svelte';
	import Logo from '$lib/ui/Logo.svelte';
	import { getVersionQuery } from '$lib/shell/version.svelte';

	const GITHUB_URL = 'https://github.com/DroppedNeedle/DroppedNeedle';

	const versionQuery = getVersionQuery();
	const version = $derived(versionQuery.data?.version ?? null);
	const versionLabel = $derived(
		version === null ? null : version === 'dev' ? 'dev' : `v${version}`
	);
</script>

<footer class="ms-footer" aria-label="Site footer">
	<div class="ms-footer__edge">
		<FritEdge place="bottom" />
	</div>
	<div class="ms-footer__bar">
		<div class="ms-footer__inner">
			<div class="ms-footer__brand">
				<Logo class="ms-footer__logo" />
				<span class="ms-footer__tag">Your Music. Your Way.</span>
			</div>
			<div class="ms-footer__links">
				<a class="ms-footer__link" href={GITHUB_URL} target="_blank" rel="noopener noreferrer">
					<GitHubIcon class="h-4 w-4" />
					<span>GitHub</span>
				</a>
				{#if versionLabel}
					<span class="ms-footer__ver">{versionLabel}</span>
				{/if}
			</div>
		</div>
	</div>
</footer>

<style>
	.ms-footer {
		margin-top: auto;
	}

	/* the frit's own strip, so it never sits under the page's last words */
	.ms-footer__edge {
		position: relative;
		height: 8rem;
		margin-top: 1rem;
	}

	.ms-footer__bar {
		padding-block: 1.25rem;
		border-top: 1px solid var(--color-base-300);
		background: var(--color-base-100);
	}

	.ms-footer__inner {
		width: 100%;
		max-width: 85rem;
		margin: 0 auto;
		padding-inline: clamp(1rem, 4vw, 2rem);
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem 1.5rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		letter-spacing: 0.06em;
		text-transform: uppercase;
	}

	.ms-footer__brand {
		display: flex;
		align-items: center;
		gap: 2rem;
	}

	.ms-footer__brand :global(.ms-footer__logo) {
		height: 2.25rem;
		color: var(--color-base-content);
	}

	.ms-footer__tag {
		color: var(--color-base-content);
	}

	.ms-footer__links {
		display: flex;
		align-items: center;
		gap: 1.5rem;
	}

	.ms-footer__link {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		color: var(--color-base-content);
		border-radius: 0.25rem;
		transition: color 150ms;
	}
	.ms-footer__link:hover {
		color: var(--color-primary);
	}

	.ms-footer__ver {
		color: var(--color-muted);
	}

	/* a phone stacks two aligned rows: the brand, then the links */
	@media (max-width: 639px) {
		.ms-footer__edge {
			height: 5rem;
		}
		.ms-footer__inner {
			flex-direction: column;
			align-items: flex-start;
			gap: 0.875rem;
			font-size: 0.75rem;
		}
		.ms-footer__brand {
			gap: 1rem;
		}
		.ms-footer__brand :global(.ms-footer__logo) {
			height: 2rem;
		}
	}
</style>
