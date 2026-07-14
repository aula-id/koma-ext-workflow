import type { Config } from 'tailwindcss';

export default {
  content: [
    './index.html',
    './src/**/*.{js,ts,jsx,tsx}',
  ],
  theme: {
    extend: {
      colors: {
        'wf-bg': '#1e1f1c',
        'wf-bg-secondary': '#26271f',
        'wf-fg': '#f8f8f2',
        'wf-accent-green': '#a6e22e',
        'wf-accent-orange': '#fd971f',
        'wf-accent-pink': '#f92672',
        'wf-accent-blue': '#66d9ef',
        'wf-accent-purple': '#ae81ff',
        'wf-accent-yellow': '#e6db74',
      },
      backgroundColor: {
        DEFAULT: '#1e1f1c',
      },
      textColor: {
        DEFAULT: '#f8f8f2',
      },
    },
  },
  plugins: [],
} satisfies Config;
