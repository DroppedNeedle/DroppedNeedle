<script lang="ts">
	import { Github } from 'lucide-svelte';
	import { getVersionQuery } from '$lib/queries/VersionQuery.svelte';

	const GITHUB_URL = 'https://github.com/HabiRabbu/DroppedNeedle';

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

<footer class="ms-footer grain" aria-label="Site footer">
	<div class="ms-footer__inner">
		<div class="ms-footer__brand">
			<img src="/logo_wide_white.png" alt="DroppedNeedle" class="ms-footer__logo" />
		</div>

		<div class="ms-footer__rule" aria-hidden="true"></div>

		<div class="ms-footer__meta">
			<span class="ms-footer__tag">Your Music. Your Way.</span>
			<div class="ms-footer__links">
				<a class="ms-footer__link" href={GITHUB_URL} target="_blank" rel="noopener noreferrer">
					<Github class="h-4 w-4" />
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
		--grain-opacity: 0.16;
		margin-top: auto;
		padding: 1.75rem 0;
		border-top: 1px solid oklch(from var(--color-base-content) l c h / 0.08);
		background: linear-gradient(
			180deg,
			oklch(from var(--color-base-100) calc(l * 0.72) c h),
			oklch(from var(--color-base-100) calc(l * 0.42) c h)
		);
		box-shadow:
			inset 0 1px 0 oklch(from var(--color-base-content) l c h / 0.05),
			inset 0 24px 48px -36px rgb(0 0 0 / 0.9);
	}

	.ms-footer__inner {
		width: 100%;
		max-width: 80rem;
		margin: 0 auto;
		padding-inline: clamp(1rem, 4vw, 2.5rem);
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.ms-footer__brand {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}

	.ms-footer__logo {
		height: clamp(2.25rem, 5vw, 2.75rem);
		width: auto;
		opacity: 0.9;
	}

	.ms-footer__rule {
		height: 2px;
		width: 100%;
		border-radius: 999px;
		background: linear-gradient(
			to right,
			transparent,
			oklch(from var(--color-primary) l c h / 0.55) 18%,
			oklch(from var(--color-accent) l c h / 0.55) 78%,
			transparent
		);
		box-shadow: 0 0 12px oklch(from var(--color-primary) l c h / 0.15);
	}

	.ms-footer__meta {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem 1.5rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.5);
	}

	.ms-footer__links {
		display: flex;
		align-items: center;
		gap: 1.25rem;
	}

	.ms-footer__link {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		color: oklch(from var(--color-base-content) l c h / 0.7);
		transition: color 0.15s ease;
	}
	.ms-footer__link:hover {
		color: oklch(from var(--color-primary) l c h);
	}
	.ms-footer__link:focus-visible {
		outline: 2px solid oklch(from var(--color-primary) l c h / 0.6);
		outline-offset: 3px;
		border-radius: 4px;
	}

	.ms-footer__ver {
		color: oklch(from var(--color-base-content) l c h / 0.4);
	}
</style>
