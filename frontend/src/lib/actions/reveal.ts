let reducedMotion =
	typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

if (typeof window !== 'undefined') {
	window.matchMedia('(prefers-reduced-motion: reduce)').addEventListener('change', (e) => {
		reducedMotion = e.matches;
	});
}

export function reveal(node: HTMLElement, options?: { threshold?: number; stagger?: number }) {
	// threshold 0, not a ratio: a ratio is unreachable for elements taller than
	// 1/ratio viewports, leaving them invisible while still occupying layout space.
	const threshold = options?.threshold ?? 0;
	const stagger = options?.stagger ?? 50;

	if (reducedMotion || typeof IntersectionObserver === 'undefined') {
		node.classList.add('revealed');
		return { destroy() {} };
	}

	node.classList.add('scroll-reveal');

	const observer = new IntersectionObserver(
		(entries) => {
			for (const entry of entries) {
				if (entry.isIntersecting) {
					const children = Array.from(node.children) as HTMLElement[];
					children.forEach((child, i) => {
						child.style.transitionDelay = `${i * stagger}ms`;
					});
					node.classList.add('revealed');
					observer.unobserve(node);
				}
			}
		},
		{ threshold }
	);

	observer.observe(node);

	return {
		destroy() {
			observer.disconnect();
			const children = Array.from(node.children) as HTMLElement[];
			children.forEach((child) => {
				child.style.transitionDelay = '';
			});
		}
	};
}
