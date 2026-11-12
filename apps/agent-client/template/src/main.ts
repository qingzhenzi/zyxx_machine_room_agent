import { createPinia } from 'pinia'
import { createApp } from 'vue'

import App from './App.vue'
import { i18n } from './i18n'
import { initFrontendLogging } from './lib/desktop'
import router from './router'
import './assets/main.css'

initFrontendLogging()

const app = createApp(App)

app.use(createPinia())
app.use(i18n)
app.use(router)
app.mount('#app')
